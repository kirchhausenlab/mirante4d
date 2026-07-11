use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

use mirante4d_core::{DatasetId, LayerId, ScaleLevel, Shape3D, TimeIndex};
use mirante4d_data::{SpatialBrickIndex, VolumeRegion};

use crate::{
    RenderError,
    resources::{ResourceRepresentation, TransformKey},
};

use super::IntegerAtlasDType;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PackedIntegerBrick {
    pub(super) values: Vec<u32>,
    pub(super) validity_bits: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum UploadReadyValidityRepresentation {
    ImplicitAllValid,
    DenseRenderValidMask { valid_voxel_count: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct UploadReadyIntegerBrickKey {
    dataset_id: DatasetId,
    layer_id: LayerId,
    scale_level: ScaleLevel,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
    region: VolumeRegion,
    atlas_brick_shape: Shape3D,
    source_brick_shape: Shape3D,
    transform: TransformKey,
    representation: ResourceRepresentation,
    validity: UploadReadyValidityRepresentation,
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

    pub(super) fn get_or_pack_u8(
        &mut self,
        brick: &mirante4d_data::VolumeBrickU8,
        atlas_brick_shape: Shape3D,
        packed_u32_per_brick: u64,
        valid_u32_per_brick: u64,
    ) -> Result<Arc<PackedIntegerBrick>, RenderError> {
        let key = UploadReadyIntegerBrickKey::from_u8(brick, atlas_brick_shape);
        self.get_or_pack(key, || {
            pack_u8_brick_for_slot(
                brick,
                atlas_brick_shape,
                packed_u32_per_brick,
                valid_u32_per_brick,
            )
        })
    }

    pub(super) fn get_or_pack_u16(
        &mut self,
        brick: &mirante4d_data::VolumeBrickU16,
        atlas_brick_shape: Shape3D,
        packed_u32_per_brick: u64,
        valid_u32_per_brick: u64,
    ) -> Result<Arc<PackedIntegerBrick>, RenderError> {
        let key = UploadReadyIntegerBrickKey::from_u16(brick, atlas_brick_shape);
        self.get_or_pack(key, || {
            pack_u16_brick_for_slot(
                brick,
                atlas_brick_shape,
                packed_u32_per_brick,
                valid_u32_per_brick,
            )
        })
    }

    fn get_or_pack(
        &mut self,
        key: UploadReadyIntegerBrickKey,
        pack: impl FnOnce() -> Result<PackedIntegerBrick, RenderError>,
    ) -> Result<Arc<PackedIntegerBrick>, RenderError> {
        if let Some(packed) = self.bricks.get(&key).cloned() {
            self.hits += 1;
            self.touch_key(&key);
            return Ok(packed);
        }

        self.misses += 1;
        let packed = Arc::new(pack()?);
        let bytes = packed_integer_brick_bytes(&packed)?;
        if bytes > self.max_bytes || self.max_bytes == 0 {
            return Ok(packed);
        }

        self.current_bytes =
            self.current_bytes
                .checked_add(bytes)
                .ok_or(RenderError::InvalidBrickAtlas(
                    "upload-ready brick cache byte count overflow",
                ))?;
        self.order.push_back(key.clone());
        self.bricks.insert(key, packed.clone());
        self.evict_to_budget()?;
        Ok(packed)
    }

    fn touch_key(&mut self, key: &UploadReadyIntegerBrickKey) {
        self.order.retain(|candidate| candidate != key);
        self.order.push_back(key.clone());
    }

    fn evict_to_budget(&mut self) -> Result<(), RenderError> {
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
                        "upload-ready brick cache byte count underflow",
                    ))?;
                self.evictions += 1;
            }
        }
        Ok(())
    }
}

impl UploadReadyIntegerBrickKey {
    fn from_u8(brick: &mirante4d_data::VolumeBrickU8, atlas_brick_shape: Shape3D) -> Self {
        Self {
            dataset_id: brick.volume.dataset_id.clone(),
            layer_id: brick.volume.layer_id.clone(),
            scale_level: ScaleLevel(brick.scale_level),
            timepoint: brick.volume.timepoint,
            brick_index: brick.brick_index,
            region: brick.region,
            atlas_brick_shape,
            source_brick_shape: brick.volume.shape,
            transform: TransformKey::from_grid_to_world(brick.volume.grid_to_world),
            representation: ResourceRepresentation::BrickedU8Atlas,
            validity: validity_representation(
                brick.volume.render_valid_mask(),
                brick.valid_voxel_count,
            ),
        }
    }

    fn from_u16(brick: &mirante4d_data::VolumeBrickU16, atlas_brick_shape: Shape3D) -> Self {
        Self {
            dataset_id: brick.volume.dataset_id.clone(),
            layer_id: brick.volume.layer_id.clone(),
            scale_level: ScaleLevel(brick.scale_level),
            timepoint: brick.volume.timepoint,
            brick_index: brick.brick_index,
            region: brick.region,
            atlas_brick_shape,
            source_brick_shape: brick.volume.shape,
            transform: TransformKey::from_grid_to_world(brick.volume.grid_to_world),
            representation: ResourceRepresentation::BrickedU16Atlas,
            validity: validity_representation(
                brick.volume.render_valid_mask(),
                brick.valid_voxel_count,
            ),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct LocalBrickOffset {
    z: u64,
    y: u64,
    x: u64,
}

fn brick_region_local_offset(
    brick_index: SpatialBrickIndex,
    region: VolumeRegion,
    brick_shape: Shape3D,
) -> Result<LocalBrickOffset, RenderError> {
    let origin_z =
        brick_index
            .z
            .checked_mul(brick_shape.z)
            .ok_or(RenderError::InvalidBrickAtlas(
                "brick z origin overflows for atlas packing",
            ))?;
    let origin_y =
        brick_index
            .y
            .checked_mul(brick_shape.y)
            .ok_or(RenderError::InvalidBrickAtlas(
                "brick y origin overflows for atlas packing",
            ))?;
    let origin_x =
        brick_index
            .x
            .checked_mul(brick_shape.x)
            .ok_or(RenderError::InvalidBrickAtlas(
                "brick x origin overflows for atlas packing",
            ))?;
    if region.z_start < origin_z || region.y_start < origin_y || region.x_start < origin_x {
        return Err(RenderError::InvalidBrickAtlas(
            "resident brick region starts before atlas brick origin",
        ));
    }
    let offset = LocalBrickOffset {
        z: region.z_start - origin_z,
        y: region.y_start - origin_y,
        x: region.x_start - origin_x,
    };
    if offset.z + region.z_size > brick_shape.z
        || offset.y + region.y_size > brick_shape.y
        || offset.x + region.x_size > brick_shape.x
    {
        return Err(RenderError::InvalidBrickAtlas(
            "resident brick region exceeds atlas brick shape",
        ));
    }
    Ok(offset)
}

pub(super) fn upload_ready_cache_budget(atlas_budget_bytes: u64) -> u64 {
    const MAX_UPLOAD_READY_CACHE_BYTES: u64 = 256 * 1024 * 1024;
    (atlas_budget_bytes / 4).min(MAX_UPLOAD_READY_CACHE_BYTES)
}

fn validity_representation(
    render_valid: Option<&[u8]>,
    valid_voxel_count: u64,
) -> UploadReadyValidityRepresentation {
    if render_valid.is_some() {
        UploadReadyValidityRepresentation::DenseRenderValidMask { valid_voxel_count }
    } else {
        UploadReadyValidityRepresentation::ImplicitAllValid
    }
}

fn packed_integer_brick_bytes(packed: &PackedIntegerBrick) -> Result<u64, RenderError> {
    let values_bytes = (packed.values.len() as u64)
        .checked_mul(std::mem::size_of::<u32>() as u64)
        .ok_or(RenderError::InvalidBrickAtlas(
            "upload-ready packed value byte count overflow",
        ))?;
    let validity_bytes = (packed.validity_bits.len() as u64)
        .checked_mul(std::mem::size_of::<u32>() as u64)
        .ok_or(RenderError::InvalidBrickAtlas(
            "upload-ready validity byte count overflow",
        ))?;
    values_bytes
        .checked_add(validity_bytes)
        .ok_or(RenderError::InvalidBrickAtlas(
            "upload-ready packed brick byte count overflow",
        ))
}

pub(super) fn pack_u8_brick_for_slot(
    brick: &mirante4d_data::VolumeBrickU8,
    brick_shape: Shape3D,
    packed_u32_per_brick: u64,
    valid_u32_per_brick: u64,
) -> Result<PackedIntegerBrick, RenderError> {
    if brick.volume.render_valid_mask().is_none() && brick.volume.shape == brick_shape {
        return pack_full_valid_integer_values(
            brick.values().iter().copied().map(u32::from),
            brick.volume.shape.element_count()?,
            IntegerAtlasDType::U8,
            packed_u32_per_brick,
            valid_u32_per_brick,
        );
    }
    let mut packed = PackedIntegerBrick {
        values: vec![0; packed_u32_per_brick as usize],
        validity_bits: vec![0; valid_u32_per_brick as usize],
    };
    let offset = brick_region_local_offset(brick.brick_index, brick.region, brick_shape)?;
    for z in 0..brick.volume.shape.z {
        for y in 0..brick.volume.shape.y {
            for x in 0..brick.volume.shape.x {
                let local_index = (((offset.z + z) * brick_shape.y + (offset.y + y))
                    * brick_shape.x
                    + (offset.x + x)) as usize;
                if let Some(value) = brick.render_voxel(z, y, x) {
                    pack_integer_sample(
                        &mut packed,
                        local_index,
                        u32::from(value),
                        IntegerAtlasDType::U8,
                    );
                }
            }
        }
    }
    Ok(packed)
}

pub(super) fn pack_u16_brick_for_slot(
    brick: &mirante4d_data::VolumeBrickU16,
    brick_shape: Shape3D,
    packed_u32_per_brick: u64,
    valid_u32_per_brick: u64,
) -> Result<PackedIntegerBrick, RenderError> {
    if brick.volume.render_valid_mask().is_none() && brick.volume.shape == brick_shape {
        return pack_full_valid_integer_values(
            brick.values().iter().copied().map(u32::from),
            brick.volume.shape.element_count()?,
            IntegerAtlasDType::U16,
            packed_u32_per_brick,
            valid_u32_per_brick,
        );
    }
    let mut packed = PackedIntegerBrick {
        values: vec![0; packed_u32_per_brick as usize],
        validity_bits: vec![0; valid_u32_per_brick as usize],
    };
    let offset = brick_region_local_offset(brick.brick_index, brick.region, brick_shape)?;
    for z in 0..brick.volume.shape.z {
        for y in 0..brick.volume.shape.y {
            for x in 0..brick.volume.shape.x {
                let local_index = (((offset.z + z) * brick_shape.y + (offset.y + y))
                    * brick_shape.x
                    + (offset.x + x)) as usize;
                if let Some(value) = brick.render_voxel(z, y, x) {
                    pack_integer_sample(
                        &mut packed,
                        local_index,
                        u32::from(value),
                        IntegerAtlasDType::U16,
                    );
                }
            }
        }
    }
    Ok(packed)
}

fn pack_full_valid_integer_values(
    values: impl Iterator<Item = u32>,
    value_count: u64,
    dtype: IntegerAtlasDType,
    packed_u32_per_brick: u64,
    valid_u32_per_brick: u64,
) -> Result<PackedIntegerBrick, RenderError> {
    let value_count_usize = usize::try_from(value_count)
        .map_err(|_| RenderError::InvalidBrickAtlas("brick value count exceeds usize"))?;
    let mut packed = PackedIntegerBrick {
        values: vec![0; packed_u32_per_brick as usize],
        validity_bits: vec![0; valid_u32_per_brick as usize],
    };
    let values_per_word = dtype.values_per_word() as usize;
    let bits_per_value = dtype.bits_per_value();
    let mask = dtype.value_mask();
    let mut seen = 0usize;
    for (index, value) in values.enumerate() {
        if index >= value_count_usize {
            return Err(RenderError::InvalidBrickAtlas(
                "brick value iterator exceeds expected value count",
            ));
        }
        seen = index + 1;
        let value_word = index / values_per_word;
        let value_shift = ((index % values_per_word) as u32) * bits_per_value;
        packed.values[value_word] |= (value & mask) << value_shift;
    }
    if seen != value_count_usize {
        return Err(RenderError::InvalidBrickAtlas(
            "brick value iterator is shorter than expected value count",
        ));
    }
    for index in 0..value_count_usize {
        let validity_word = index / 32;
        let validity_bit = index % 32;
        packed.validity_bits[validity_word] |= 1u32 << validity_bit;
    }
    Ok(packed)
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
    packed.validity_bits[validity_word] |= 1u32 << validity_bit;
}

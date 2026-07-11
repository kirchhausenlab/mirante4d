use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::gpu) struct GpuBrickAtlas {
    pub(in crate::gpu) packed_values: Vec<u32>,
    pub(in crate::gpu) validity_bits: Vec<u32>,
    pub(in crate::gpu) page_table: Vec<u32>,
    pub(in crate::gpu) brick_shape: Shape3D,
    pub(in crate::gpu) brick_grid_shape: Shape3D,
    pub(in crate::gpu) brick_voxel_count: u64,
    pub(in crate::gpu) packed_u32_per_brick: u64,
    pub(in crate::gpu) valid_u32_per_brick: u64,
}

pub(in crate::gpu) fn build_gpu_brick_atlas(
    resident: &ResidentBrickSetU16,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
) -> Result<GpuBrickAtlas, RenderError> {
    brick_shape
        .element_count()
        .map_err(RenderError::from)
        .and_then(|count| {
            if count == 0 {
                Err(RenderError::InvalidBrickAtlas(
                    "brick shape has zero voxels",
                ))
            } else {
                Ok(count)
            }
        })?;
    let brick_voxel_count = brick_shape.element_count()?;
    let pages = validate_resident_pages_u16(
        resident,
        brick_shape,
        brick_grid_shape,
        resident.bricks().len(),
    )?;
    let page_count = brick_grid_shape.element_count()? as usize;
    let packed_u32_per_brick = IntegerAtlasDType::U16.packed_u32_per_brick(brick_voxel_count);
    let valid_u32_per_brick = validity_u32_per_brick(brick_voxel_count);
    let atlas_value_count = resident
        .bricks()
        .len()
        .checked_mul(packed_u32_per_brick as usize)
        .ok_or(RenderError::InvalidBrickAtlas(
            "brick atlas value count overflow",
        ))?;
    let atlas_validity_count = resident
        .bricks()
        .len()
        .checked_mul(valid_u32_per_brick as usize)
        .ok_or(RenderError::InvalidBrickAtlas(
            "brick atlas validity count overflow",
        ))?;
    let mut packed_values = vec![0u32; atlas_value_count];
    let mut validity_bits = vec![0u32; atlas_validity_count];
    let mut page_table = vec![0u32; page_count];

    for (slot, brick) in resident.bricks().iter().enumerate() {
        debug_assert!(pages.contains(&brick.brick_index));
        let page_index = brick_page_index(brick.brick_index, brick_grid_shape);
        page_table[page_index] = u32::try_from(slot + 1)
            .map_err(|_| RenderError::InvalidBrickAtlas("brick atlas slot exceeds u32"))?;
        let packed = pack_u16_brick_for_slot(
            brick,
            brick_shape,
            packed_u32_per_brick,
            valid_u32_per_brick,
        )?;
        let value_slot_offset = slot * packed_u32_per_brick as usize;
        packed_values[value_slot_offset..value_slot_offset + packed.values.len()]
            .copy_from_slice(&packed.values);
        let validity_slot_offset = slot * valid_u32_per_brick as usize;
        validity_bits[validity_slot_offset..validity_slot_offset + packed.validity_bits.len()]
            .copy_from_slice(&packed.validity_bits);
    }

    Ok(GpuBrickAtlas {
        packed_values,
        validity_bits,
        page_table,
        brick_shape,
        brick_grid_shape,
        brick_voxel_count,
        packed_u32_per_brick,
        valid_u32_per_brick,
    })
}

pub(in crate::gpu) fn build_gpu_brick_atlas_u8(
    resident: &ResidentBrickSetU8,
    brick_shape: Shape3D,
    brick_grid_shape: Shape3D,
) -> Result<GpuBrickAtlas, RenderError> {
    brick_shape
        .element_count()
        .map_err(RenderError::from)
        .and_then(|count| {
            if count == 0 {
                Err(RenderError::InvalidBrickAtlas(
                    "brick shape has zero voxels",
                ))
            } else {
                Ok(count)
            }
        })?;
    let brick_voxel_count = brick_shape.element_count()?;
    let pages = validate_resident_pages_u8(
        resident,
        brick_shape,
        brick_grid_shape,
        resident.bricks().len(),
    )?;
    let page_count = brick_grid_shape.element_count()? as usize;
    let packed_u32_per_brick = IntegerAtlasDType::U8.packed_u32_per_brick(brick_voxel_count);
    let valid_u32_per_brick = validity_u32_per_brick(brick_voxel_count);
    let atlas_value_count = resident
        .bricks()
        .len()
        .checked_mul(packed_u32_per_brick as usize)
        .ok_or(RenderError::InvalidBrickAtlas(
            "brick atlas value count overflow",
        ))?;
    let atlas_validity_count = resident
        .bricks()
        .len()
        .checked_mul(valid_u32_per_brick as usize)
        .ok_or(RenderError::InvalidBrickAtlas(
            "brick atlas validity count overflow",
        ))?;
    let mut packed_values = vec![0u32; atlas_value_count];
    let mut validity_bits = vec![0u32; atlas_validity_count];
    let mut page_table = vec![0u32; page_count];

    for (slot, brick) in resident.bricks().iter().enumerate() {
        debug_assert!(pages.contains(&brick.brick_index));
        let page_index = brick_page_index(brick.brick_index, brick_grid_shape);
        page_table[page_index] = u32::try_from(slot + 1)
            .map_err(|_| RenderError::InvalidBrickAtlas("brick atlas slot exceeds u32"))?;
        let packed = pack_u8_brick_for_slot(
            brick,
            brick_shape,
            packed_u32_per_brick,
            valid_u32_per_brick,
        )?;
        let value_slot_offset = slot * packed_u32_per_brick as usize;
        packed_values[value_slot_offset..value_slot_offset + packed.values.len()]
            .copy_from_slice(&packed.values);
        let validity_slot_offset = slot * valid_u32_per_brick as usize;
        validity_bits[validity_slot_offset..validity_slot_offset + packed.validity_bits.len()]
            .copy_from_slice(&packed.validity_bits);
    }

    Ok(GpuBrickAtlas {
        packed_values,
        validity_bits,
        page_table,
        brick_shape,
        brick_grid_shape,
        brick_voxel_count,
        packed_u32_per_brick,
        valid_u32_per_brick,
    })
}

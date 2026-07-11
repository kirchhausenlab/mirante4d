use mirante4d_core::{DatasetId, LayerId, Shape3D, TimeIndex};
use mirante4d_format::ScaleManifest;

use crate::regions::{brick_record_and_region, translated_region_grid_to_world};
use crate::{
    BrickCacheKey, DataError, DenseVolumeF32, DenseVolumeU8, DenseVolumeU16, SpatialBrickIndex,
    VolumeBrickF32, VolumeBrickU8, VolumeBrickU16, VolumeRegion,
};

pub(super) fn brick_cache_key(
    layer_id: &LayerId,
    scale_level: u32,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
) -> BrickCacheKey {
    BrickCacheKey {
        layer_id: layer_id.to_string(),
        scale_level,
        timepoint: timepoint.0,
        z: brick_index.z,
        y: brick_index.y,
        x: brick_index.x,
    }
}

pub(super) fn storage_shard_brick_indices(
    scale: &ScaleManifest,
    shard_index: SpatialBrickIndex,
) -> Result<Vec<SpatialBrickIndex>, DataError> {
    let chunks_per_shard = scale.storage.chunks_per_shard;
    let grid = scale.bricks.grid_shape.spatial();
    let z_start = shard_index.z.saturating_mul(chunks_per_shard.z);
    let y_start = shard_index.y.saturating_mul(chunks_per_shard.y);
    let x_start = shard_index.x.saturating_mul(chunks_per_shard.x);
    let z_end = z_start.saturating_add(chunks_per_shard.z).min(grid.z);
    let y_end = y_start.saturating_add(chunks_per_shard.y).min(grid.y);
    let x_end = x_start.saturating_add(chunks_per_shard.x).min(grid.x);
    if z_start >= z_end || y_start >= y_end || x_start >= x_end {
        return Err(DataError::BrickIndexOutOfBounds {
            z: z_start,
            y: y_start,
            x: x_start,
            grid_z: grid.z,
            grid_y: grid.y,
            grid_x: grid.x,
        });
    }
    let mut bricks = Vec::new();
    for z in z_start..z_end {
        for y in y_start..y_end {
            for x in x_start..x_end {
                bricks.push(SpatialBrickIndex { z, y, x });
            }
        }
    }
    Ok(bricks)
}

pub(super) fn storage_shard_region(
    scale: &ScaleManifest,
    shard_index: SpatialBrickIndex,
) -> Result<VolumeRegion, DataError> {
    let chunks_per_shard = scale.storage.chunks_per_shard;
    let brick_shape = scale.storage.brick_shape;
    let spatial = scale.shape.spatial();
    let z_start = shard_index
        .z
        .saturating_mul(chunks_per_shard.z)
        .saturating_mul(brick_shape.z);
    let y_start = shard_index
        .y
        .saturating_mul(chunks_per_shard.y)
        .saturating_mul(brick_shape.y);
    let x_start = shard_index
        .x
        .saturating_mul(chunks_per_shard.x)
        .saturating_mul(brick_shape.x);
    if z_start >= spatial.z || y_start >= spatial.y || x_start >= spatial.x {
        return Err(DataError::RegionOutOfBounds {
            z_start,
            z_end: z_start,
            y_start,
            y_end: y_start,
            x_start,
            x_end: x_start,
            shape_z: spatial.z,
            shape_y: spatial.y,
            shape_x: spatial.x,
        });
    }
    VolumeRegion::new(
        z_start,
        y_start,
        x_start,
        chunks_per_shard
            .z
            .saturating_mul(brick_shape.z)
            .min(spatial.z - z_start),
        chunks_per_shard
            .y
            .saturating_mul(brick_shape.y)
            .min(spatial.y - y_start),
        chunks_per_shard
            .x
            .saturating_mul(brick_shape.x)
            .min(spatial.x - x_start),
    )
}

#[derive(Debug, Clone, Copy)]
pub(super) struct ShardSplitContext<'a> {
    pub(super) dataset_id: &'a DatasetId,
    pub(super) layer_id: &'a LayerId,
    pub(super) scale: &'a ScaleManifest,
    pub(super) scale_level: u32,
    pub(super) timepoint: TimeIndex,
    pub(super) shard_region: VolumeRegion,
}

pub(super) fn split_u8_shard_brick(
    context: ShardSplitContext<'_>,
    shard_volume: &DenseVolumeU8,
    brick_index: SpatialBrickIndex,
) -> Result<VolumeBrickU8, DataError> {
    let (chunk_index, record, region) = brick_record_and_region(
        context.layer_id,
        context.scale,
        context.timepoint,
        brick_index,
    )?;
    let values = extract_shard_values(
        shard_volume.values(),
        shard_volume.shape,
        context.shard_region,
        region,
    )?;
    let render_valid = extract_optional_shard_mask(
        shard_volume.render_valid_mask(),
        shard_volume.shape,
        context.shard_region,
        region,
    )?;
    let shape = region.shape().map_err(DataError::InvalidShape)?;
    Ok(VolumeBrickU8 {
        scale_level: context.scale_level,
        brick_index,
        chunk_index,
        region,
        occupied: record.occupied,
        valid_voxel_count: record.valid_voxel_count,
        min: record.min,
        max: record.max,
        volume: DenseVolumeU8 {
            dataset_id: (*context.dataset_id).clone(),
            layer_id: (*context.layer_id).clone(),
            scale_level: context.scale_level,
            timepoint: context.timepoint,
            shape,
            grid_to_world: translated_region_grid_to_world(context.scale.grid_to_world, region),
            values,
            render_valid,
        },
    })
}

pub(super) fn split_u16_shard_brick(
    context: ShardSplitContext<'_>,
    shard_volume: &DenseVolumeU16,
    brick_index: SpatialBrickIndex,
) -> Result<VolumeBrickU16, DataError> {
    let (chunk_index, record, region) = brick_record_and_region(
        context.layer_id,
        context.scale,
        context.timepoint,
        brick_index,
    )?;
    let values = extract_shard_values(
        shard_volume.values(),
        shard_volume.shape,
        context.shard_region,
        region,
    )?;
    let render_valid = extract_optional_shard_mask(
        shard_volume.render_valid_mask(),
        shard_volume.shape,
        context.shard_region,
        region,
    )?;
    let shape = region.shape().map_err(DataError::InvalidShape)?;
    Ok(VolumeBrickU16 {
        scale_level: context.scale_level,
        brick_index,
        chunk_index,
        region,
        occupied: record.occupied,
        valid_voxel_count: record.valid_voxel_count,
        min: record.min,
        max: record.max,
        volume: DenseVolumeU16 {
            dataset_id: (*context.dataset_id).clone(),
            layer_id: (*context.layer_id).clone(),
            scale_level: context.scale_level,
            timepoint: context.timepoint,
            shape,
            grid_to_world: translated_region_grid_to_world(context.scale.grid_to_world, region),
            values,
            render_valid,
        },
    })
}

pub(super) fn split_f32_shard_brick(
    context: ShardSplitContext<'_>,
    shard_volume: &DenseVolumeF32,
    brick_index: SpatialBrickIndex,
) -> Result<VolumeBrickF32, DataError> {
    let (chunk_index, record, region) = brick_record_and_region(
        context.layer_id,
        context.scale,
        context.timepoint,
        brick_index,
    )?;
    let values = extract_shard_values(
        shard_volume.values(),
        shard_volume.shape,
        context.shard_region,
        region,
    )?;
    let render_valid = extract_optional_shard_mask(
        shard_volume.render_valid_mask(),
        shard_volume.shape,
        context.shard_region,
        region,
    )?;
    let shape = region.shape().map_err(DataError::InvalidShape)?;
    Ok(VolumeBrickF32 {
        scale_level: context.scale_level,
        brick_index,
        chunk_index,
        region,
        occupied: record.occupied,
        valid_voxel_count: record.valid_voxel_count,
        min: record.min,
        max: record.max,
        volume: DenseVolumeF32 {
            dataset_id: (*context.dataset_id).clone(),
            layer_id: (*context.layer_id).clone(),
            scale_level: context.scale_level,
            timepoint: context.timepoint,
            shape,
            grid_to_world: translated_region_grid_to_world(context.scale.grid_to_world, region),
            values,
            render_valid,
        },
    })
}

fn extract_optional_shard_mask(
    mask: Option<&[u8]>,
    shard_shape: Shape3D,
    shard_region: VolumeRegion,
    target_region: VolumeRegion,
) -> Result<Option<Vec<u8>>, DataError> {
    mask.map(|mask| extract_shard_values(mask, shard_shape, shard_region, target_region))
        .transpose()
}

fn extract_shard_values<T: Copy>(
    values: &[T],
    shard_shape: Shape3D,
    shard_region: VolumeRegion,
    target_region: VolumeRegion,
) -> Result<Vec<T>, DataError> {
    validate_region_inside_region(target_region, shard_region)?;
    let shape = target_region.shape().map_err(DataError::InvalidShape)?;
    let expected = shard_shape
        .element_count()
        .map_err(DataError::InvalidShape)? as usize;
    if values.len() != expected {
        return Err(DataError::ReadFailed {
            layer_id: "<shard>".to_owned(),
            message: format!(
                "decoded shard has {} values, expected {expected}",
                values.len()
            ),
        });
    }
    let z_offset = target_region.z_start - shard_region.z_start;
    let y_offset = target_region.y_start - shard_region.y_start;
    let x_offset = target_region.x_start - shard_region.x_start;
    let mut extracted =
        Vec::with_capacity(shape.element_count().map_err(DataError::InvalidShape)? as usize);
    for z in 0..shape.z {
        for y in 0..shape.y {
            let start = (((z_offset + z) * shard_shape.y + (y_offset + y)) * shard_shape.x
                + x_offset) as usize;
            let end = start + shape.x as usize;
            extracted.extend_from_slice(values.get(start..end).ok_or_else(|| {
                DataError::ReadFailed {
                    layer_id: "<shard>".to_owned(),
                    message: "target brick range exceeds decoded shard".to_owned(),
                }
            })?);
        }
    }
    Ok(extracted)
}

fn validate_region_inside_region(
    inner: VolumeRegion,
    outer: VolumeRegion,
) -> Result<(), DataError> {
    let inner_ends = inner.ends()?;
    let outer_ends = outer.ends()?;
    if inner.z_start < outer.z_start
        || inner.y_start < outer.y_start
        || inner.x_start < outer.x_start
        || inner_ends.z > outer_ends.z
        || inner_ends.y > outer_ends.y
        || inner_ends.x > outer_ends.x
    {
        return Err(DataError::RegionOutOfBounds {
            z_start: inner.z_start,
            z_end: inner_ends.z,
            y_start: inner.y_start,
            y_end: inner_ends.y,
            x_start: inner.x_start,
            x_end: inner_ends.x,
            shape_z: outer_ends.z,
            shape_y: outer_ends.y,
            shape_x: outer_ends.x,
        });
    }
    Ok(())
}

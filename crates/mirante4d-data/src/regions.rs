use glam::{DMat4, DVec3};
use mirante4d_core::{GridToWorld, LayerId, TimeIndex};
use mirante4d_format::{BrickIndex, BrickRecord, LayerManifest, ScaleManifest};

use crate::{DataError, SpatialBrickIndex, VolumeRegion};

pub(super) fn validate_spatial_brick_index(
    scale: &ScaleManifest,
    brick_index: SpatialBrickIndex,
) -> Result<(), DataError> {
    let grid = scale.bricks.grid_shape.spatial();
    if brick_index.z >= grid.z || brick_index.y >= grid.y || brick_index.x >= grid.x {
        return Err(DataError::BrickIndexOutOfBounds {
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
            grid_z: grid.z,
            grid_y: grid.y,
            grid_x: grid.x,
        });
    }
    Ok(())
}

pub(super) fn brick_record(
    layer_id: &LayerId,
    scale: &ScaleManifest,
    index: BrickIndex,
) -> Result<BrickRecord, DataError> {
    scale
        .bricks
        .records
        .iter()
        .find(|record| record.index == index)
        .cloned()
        .ok_or_else(|| DataError::BrickRecordMissing {
            layer_id: layer_id.to_string(),
            index,
        })
}

pub(super) fn brick_record_and_region(
    layer_id: &LayerId,
    scale: &ScaleManifest,
    timepoint: TimeIndex,
    brick_index: SpatialBrickIndex,
) -> Result<(BrickIndex, BrickRecord, VolumeRegion), DataError> {
    validate_spatial_brick_index(scale, brick_index)?;
    let chunk_index = BrickIndex {
        t: timepoint.0 / scale.storage.brick_shape.t,
        z: brick_index.z,
        y: brick_index.y,
        x: brick_index.x,
    };
    let record = brick_record(layer_id, scale, chunk_index)?;
    let region = brick_region(scale, brick_index)?;
    Ok((chunk_index, record, region))
}

pub(super) fn brick_region(
    scale: &ScaleManifest,
    brick_index: SpatialBrickIndex,
) -> Result<VolumeRegion, DataError> {
    let brick_shape = scale.storage.brick_shape;
    let z_start = brick_index.z * brick_shape.z;
    let y_start = brick_index.y * brick_shape.y;
    let x_start = brick_index.x * brick_shape.x;
    let spatial = scale.shape.spatial();
    VolumeRegion::new(
        z_start,
        y_start,
        x_start,
        brick_shape.z.min(spatial.z - z_start),
        brick_shape.y.min(spatial.y - y_start),
        brick_shape.x.min(spatial.x - x_start),
    )
}

pub(super) fn validate_region_within_brick(
    region: VolumeRegion,
    brick_region: VolumeRegion,
) -> Result<(), DataError> {
    let ends = region.ends()?;
    let brick_ends = brick_region.ends()?;
    if region.z_start < brick_region.z_start
        || region.y_start < brick_region.y_start
        || region.x_start < brick_region.x_start
        || ends.z > brick_ends.z
        || ends.y > brick_ends.y
        || ends.x > brick_ends.x
    {
        return Err(DataError::BrickRegionOutOfBounds {
            z_start: region.z_start,
            z_end: ends.z,
            y_start: region.y_start,
            y_end: ends.y,
            x_start: region.x_start,
            x_end: ends.x,
            brick_z_start: brick_region.z_start,
            brick_z_end: brick_ends.z,
            brick_y_start: brick_region.y_start,
            brick_y_end: brick_ends.y,
            brick_x_start: brick_region.x_start,
            brick_x_end: brick_ends.x,
        });
    }
    Ok(())
}

pub(super) fn validate_timepoint(
    layer_id: &LayerId,
    layer: &LayerManifest,
    timepoint: TimeIndex,
) -> Result<(), DataError> {
    if timepoint.0 >= layer.shape.t {
        return Err(DataError::TimepointOutOfRange {
            layer_id: layer_id.to_string(),
            timepoint: timepoint.0,
            timepoints: layer.shape.t,
        });
    }
    Ok(())
}

pub fn translated_region_grid_to_world(
    grid_to_world: GridToWorld,
    region: VolumeRegion,
) -> GridToWorld {
    let local_to_full_grid = DMat4::from_translation(DVec3::new(
        region.x_start as f64,
        region.y_start as f64,
        region.z_start as f64,
    ));
    GridToWorld::from_dmat4(grid_to_world.to_dmat4() * local_to_full_grid)
}

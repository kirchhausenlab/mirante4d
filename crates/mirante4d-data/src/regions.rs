use glam::{DMat4, DVec3};
use mirante4d_domain::{GridToWorld, TimeIndex};
use mirante4d_format::{
    BrickIndex, BrickRecord, CurrentGridToWorldExt, LayerId, LayerManifest, ScaleManifest,
    grid_to_world_from_dmat4,
};

use crate::{DataError, SpatialBrickIndex, VolumeRegion};

pub(super) fn validate_spatial_brick_index(
    scale: &ScaleManifest,
    brick_index: SpatialBrickIndex,
) -> Result<(), DataError> {
    let grid = scale.bricks.grid_shape.spatial();
    if brick_index.z >= grid.z() || brick_index.y >= grid.y() || brick_index.x >= grid.x() {
        return Err(DataError::BrickIndexOutOfBounds {
            z: brick_index.z,
            y: brick_index.y,
            x: brick_index.x,
            grid_z: grid.z(),
            grid_y: grid.y(),
            grid_x: grid.x(),
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

pub(super) fn brick_region(
    scale: &ScaleManifest,
    brick_index: SpatialBrickIndex,
) -> Result<VolumeRegion, DataError> {
    let brick_shape = scale.storage.brick_shape;
    let z_start = brick_index.z * brick_shape.z();
    let y_start = brick_index.y * brick_shape.y();
    let x_start = brick_index.x * brick_shape.x();
    let spatial = scale.shape.spatial();
    VolumeRegion::new(
        z_start,
        y_start,
        x_start,
        brick_shape.z().min(spatial.z() - z_start),
        brick_shape.y().min(spatial.y() - y_start),
        brick_shape.x().min(spatial.x() - x_start),
    )
}

pub(super) fn validate_timepoint(
    layer_id: &LayerId,
    layer: &LayerManifest,
    timepoint: TimeIndex,
) -> Result<(), DataError> {
    if timepoint.get() >= layer.shape.t() {
        return Err(DataError::TimepointOutOfRange {
            layer_id: layer_id.to_string(),
            timepoint: timepoint.get(),
            timepoints: layer.shape.t(),
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
    grid_to_world_from_dmat4(grid_to_world.to_dmat4() * local_to_full_grid)
        .expect("translation preserves an affine current-profile transform")
}

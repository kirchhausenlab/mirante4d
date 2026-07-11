use mirante4d_format::ScaleManifest;

use crate::{DataError, SpatialBrickIndex, VolumeRegion};

pub(super) fn storage_shard_region(
    scale: &ScaleManifest,
    shard_index: SpatialBrickIndex,
) -> Result<VolumeRegion, DataError> {
    let chunks_per_shard = scale.storage.chunks_per_shard;
    let brick_shape = scale.storage.brick_shape;
    let spatial = scale.shape.spatial();
    let z_start = shard_index
        .z
        .saturating_mul(chunks_per_shard.z())
        .saturating_mul(brick_shape.z());
    let y_start = shard_index
        .y
        .saturating_mul(chunks_per_shard.y())
        .saturating_mul(brick_shape.y());
    let x_start = shard_index
        .x
        .saturating_mul(chunks_per_shard.x())
        .saturating_mul(brick_shape.x());
    if z_start >= spatial.z() || y_start >= spatial.y() || x_start >= spatial.x() {
        return Err(DataError::RegionOutOfBounds {
            z_start,
            z_end: z_start,
            y_start,
            y_end: y_start,
            x_start,
            x_end: x_start,
            shape_z: spatial.z(),
            shape_y: spatial.y(),
            shape_x: spatial.x(),
        });
    }
    VolumeRegion::new(
        z_start,
        y_start,
        x_start,
        chunks_per_shard
            .z()
            .saturating_mul(brick_shape.z())
            .min(spatial.z() - z_start),
        chunks_per_shard
            .y()
            .saturating_mul(brick_shape.y())
            .min(spatial.y() - y_start),
        chunks_per_shard
            .x()
            .saturating_mul(brick_shape.x())
            .min(spatial.x() - x_start),
    )
}

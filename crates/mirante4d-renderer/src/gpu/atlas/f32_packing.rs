#[cfg(test)]
use mirante4d_data::{SpatialBrickIndex, VolumeRegion};
#[cfg(test)]
use mirante4d_domain::Shape3D;

use crate::RenderError;

#[cfg(test)]
pub(in crate::gpu) fn pack_brick_f32_for_slot(
    brick: &mirante4d_data::VolumeBrickF32,
    brick_shape: Shape3D,
    brick_voxel_count: u64,
) -> Result<Vec<f32>, RenderError> {
    let mut values = vec![f32::NAN; brick_voxel_count as usize];
    let offset = brick_region_local_offset(brick.brick_index, brick.region, brick_shape)?;
    for z in 0..brick.volume.shape.z() {
        for y in 0..brick.volume.shape.y() {
            for x in 0..brick.volume.shape.x() {
                let local_index = (((offset.z + z) * brick_shape.y() + (offset.y + y))
                    * brick_shape.x()
                    + (offset.x + x)) as usize;
                values[local_index] = brick.render_voxel(z, y, x).unwrap_or(f32::NAN);
            }
        }
    }
    Ok(values)
}

#[cfg(test)]
fn brick_region_local_offset(
    brick_index: SpatialBrickIndex,
    region: VolumeRegion,
    brick_shape: Shape3D,
) -> Result<GridLocalOffset, RenderError> {
    let origin_z =
        brick_index
            .z
            .checked_mul(brick_shape.z())
            .ok_or(RenderError::InvalidBrickAtlas(
                "brick z origin overflows for atlas packing",
            ))?;
    let origin_y =
        brick_index
            .y
            .checked_mul(brick_shape.y())
            .ok_or(RenderError::InvalidBrickAtlas(
                "brick y origin overflows for atlas packing",
            ))?;
    let origin_x =
        brick_index
            .x
            .checked_mul(brick_shape.x())
            .ok_or(RenderError::InvalidBrickAtlas(
                "brick x origin overflows for atlas packing",
            ))?;
    if region.z_start < origin_z || region.y_start < origin_y || region.x_start < origin_x {
        return Err(RenderError::InvalidBrickAtlas(
            "resident brick region starts before atlas brick origin",
        ));
    }
    let offset = GridLocalOffset {
        z: region.z_start - origin_z,
        y: region.y_start - origin_y,
        x: region.x_start - origin_x,
    };
    if offset.z + region.z_size > brick_shape.z()
        || offset.y + region.y_size > brick_shape.y()
        || offset.x + region.x_size > brick_shape.x()
    {
        return Err(RenderError::InvalidBrickAtlas(
            "resident brick region exceeds atlas brick shape",
        ));
    }
    Ok(offset)
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct GridLocalOffset {
    z: u64,
    y: u64,
    x: u64,
}

pub(super) fn pack_brick_f32_compact(
    brick: &mirante4d_data::VolumeBrickF32,
) -> Result<Vec<f32>, RenderError> {
    let expected = brick.volume.shape.element_count()? as usize;
    if brick.volume.values().len() != expected {
        return Err(RenderError::InvalidBrickAtlas(
            "float32 brick value count does not match brick shape",
        ));
    }
    let mut values = Vec::with_capacity(expected);
    for z in 0..brick.volume.shape.z() {
        for y in 0..brick.volume.shape.y() {
            for x in 0..brick.volume.shape.x() {
                values.push(brick.render_voxel(z, y, x).unwrap_or(f32::NAN));
            }
        }
    }
    Ok(values)
}

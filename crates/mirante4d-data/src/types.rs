use mirante4d_core::{DatasetId, GridToWorld, LayerId, Shape3D, ShapeError, TimeIndex};
use mirante4d_format::BrickIndex;

use super::DataError;

#[derive(Debug, Clone)]
pub struct DenseVolumeU8 {
    pub dataset_id: DatasetId,
    pub layer_id: LayerId,
    pub scale_level: u32,
    pub timepoint: TimeIndex,
    pub shape: Shape3D,
    pub grid_to_world: GridToWorld,
    pub(super) values: Vec<u8>,
    pub(super) render_valid: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct DenseVolumeU16 {
    pub dataset_id: DatasetId,
    pub layer_id: LayerId,
    pub scale_level: u32,
    pub timepoint: TimeIndex,
    pub shape: Shape3D,
    pub grid_to_world: GridToWorld,
    pub(super) values: Vec<u16>,
    pub(super) render_valid: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct DenseVolumeF32 {
    pub dataset_id: DatasetId,
    pub layer_id: LayerId,
    pub scale_level: u32,
    pub timepoint: TimeIndex,
    pub shape: Shape3D,
    pub grid_to_world: GridToWorld,
    pub(super) values: Vec<f32>,
    pub(super) render_valid: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct VolumeBrickU8 {
    pub scale_level: u32,
    pub brick_index: SpatialBrickIndex,
    pub chunk_index: BrickIndex,
    pub region: VolumeRegion,
    pub occupied: bool,
    pub valid_voxel_count: u64,
    pub min: f64,
    pub max: f64,
    pub volume: DenseVolumeU8,
}

#[derive(Debug, Clone)]
pub struct VolumeBrickU16 {
    pub scale_level: u32,
    pub brick_index: SpatialBrickIndex,
    pub chunk_index: BrickIndex,
    pub region: VolumeRegion,
    pub occupied: bool,
    pub valid_voxel_count: u64,
    pub min: f64,
    pub max: f64,
    pub volume: DenseVolumeU16,
}

#[derive(Debug, Clone)]
pub struct VolumeBrickF32 {
    pub scale_level: u32,
    pub brick_index: SpatialBrickIndex,
    pub chunk_index: BrickIndex,
    pub region: VolumeRegion,
    pub occupied: bool,
    pub valid_voxel_count: u64,
    pub min: f64,
    pub max: f64,
    pub volume: DenseVolumeF32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BrickMetadata {
    pub scale_level: u32,
    pub brick_index: SpatialBrickIndex,
    pub chunk_index: BrickIndex,
    pub region: VolumeRegion,
    pub grid_to_world: GridToWorld,
    pub occupied: bool,
    pub valid_voxel_count: u64,
    pub min: f64,
    pub max: f64,
    pub payload_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpatialBrickIndex {
    pub z: u64,
    pub y: u64,
    pub x: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VolumeRegion {
    pub z_start: u64,
    pub y_start: u64,
    pub x_start: u64,
    pub z_size: u64,
    pub y_size: u64,
    pub x_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RegionEnds {
    pub(super) z: u64,
    pub(super) y: u64,
    pub(super) x: u64,
}

impl VolumeRegion {
    pub fn new(
        z_start: u64,
        y_start: u64,
        x_start: u64,
        z_size: u64,
        y_size: u64,
        x_size: u64,
    ) -> Result<Self, DataError> {
        if z_size == 0 || y_size == 0 || x_size == 0 {
            return Err(DataError::InvalidRegionSize {
                z_size,
                y_size,
                x_size,
            });
        }
        Ok(Self {
            z_start,
            y_start,
            x_start,
            z_size,
            y_size,
            x_size,
        })
    }

    pub fn shape(self) -> Result<Shape3D, ShapeError> {
        Shape3D::new(self.z_size, self.y_size, self.x_size)
    }

    pub(super) fn ends(self) -> Result<RegionEnds, DataError> {
        Ok(RegionEnds {
            z: self
                .z_start
                .checked_add(self.z_size)
                .ok_or(DataError::RegionOverflow)?,
            y: self
                .y_start
                .checked_add(self.y_size)
                .ok_or(DataError::RegionOverflow)?,
            x: self
                .x_start
                .checked_add(self.x_size)
                .ok_or(DataError::RegionOverflow)?,
        })
    }

    pub fn z_end(self) -> Result<u64, DataError> {
        Ok(self.ends()?.z)
    }

    pub fn y_end(self) -> Result<u64, DataError> {
        Ok(self.ends()?.y)
    }

    pub fn x_end(self) -> Result<u64, DataError> {
        Ok(self.ends()?.x)
    }

    pub(super) fn validate_within(self, shape: Shape3D) -> Result<RegionEnds, DataError> {
        let ends = self.ends()?;
        if ends.z > shape.z || ends.y > shape.y || ends.x > shape.x {
            return Err(DataError::RegionOutOfBounds {
                z_start: self.z_start,
                z_end: ends.z,
                y_start: self.y_start,
                y_end: ends.y,
                x_start: self.x_start,
                x_end: ends.x,
                shape_z: shape.z,
                shape_y: shape.y,
                shape_x: shape.x,
            });
        }
        Ok(ends)
    }
}

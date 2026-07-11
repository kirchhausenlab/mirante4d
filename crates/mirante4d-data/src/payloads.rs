use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CacheUpdate {
    pub(super) evictions: u64,
    pub(super) current_bytes: u64,
    pub(super) current_u8_bytes: u64,
    pub(super) current_u16_bytes: u64,
    pub(super) current_f32_bytes: u64,
}

pub(super) fn u8_volume_byte_len(volume: &DenseVolumeU8) -> u64 {
    volume.values.len() as u64
        + volume
            .render_valid
            .as_ref()
            .map(|mask| mask.len() as u64)
            .unwrap_or(0)
}

pub(super) fn u16_volume_byte_len(volume: &DenseVolumeU16) -> u64 {
    (volume.values.len() * std::mem::size_of::<u16>()) as u64
        + volume
            .render_valid
            .as_ref()
            .map(|mask| mask.len() as u64)
            .unwrap_or(0)
}

pub(super) fn f32_volume_byte_len(volume: &DenseVolumeF32) -> u64 {
    (volume.values.len() * std::mem::size_of::<f32>()) as u64
        + volume
            .render_valid
            .as_ref()
            .map(|mask| mask.len() as u64)
            .unwrap_or(0)
}

pub(super) fn u8_brick_byte_len(brick: &VolumeBrickU8) -> u64 {
    u8_volume_byte_len(&brick.volume)
}

pub(super) fn u16_brick_byte_len(brick: &VolumeBrickU16) -> u64 {
    u16_volume_byte_len(&brick.volume)
}

pub(super) fn f32_brick_byte_len(brick: &VolumeBrickF32) -> u64 {
    f32_volume_byte_len(&brick.volume)
}

pub(super) fn brick_payload_byte_len(payload: &CachedBrickPayload) -> u64 {
    match payload {
        CachedBrickPayload::U8(brick) => u8_brick_byte_len(brick),
        CachedBrickPayload::U16(brick) => u16_brick_byte_len(brick),
        CachedBrickPayload::F32(brick) => f32_brick_byte_len(brick),
    }
}

impl DenseVolumeU8 {
    pub fn new(
        dataset_id: DatasetId,
        layer_id: LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        shape: Shape3D,
        grid_to_world: GridToWorld,
        values: Vec<u8>,
    ) -> Result<Self, DataError> {
        let expected = shape
            .element_count()
            .map_err(DataError::InvalidShape)
            .and_then(|count| {
                usize::try_from(count).map_err(|err| DataError::ReadFailed {
                    layer_id: layer_id.to_string(),
                    message: err.to_string(),
                })
            })?;
        if values.len() != expected {
            return Err(DataError::VolumeValueCountMismatch {
                layer_id: layer_id.to_string(),
                actual: values.len(),
                expected,
            });
        }
        Ok(Self {
            dataset_id,
            layer_id,
            scale_level,
            timepoint,
            shape,
            grid_to_world,
            values,
            render_valid: None,
        })
    }

    pub fn with_render_valid(mut self, render_valid: Vec<u8>) -> Result<Self, DataError> {
        validate_render_valid_len(&self.layer_id, Some(&render_valid), self.values.len())?;
        if render_valid.iter().any(|value| !matches!(value, 0 | 1)) {
            return Err(DataError::ReadFailed {
                layer_id: self.layer_id.to_string(),
                message: "render-valid mask contains values other than 0 or 1".to_owned(),
            });
        }
        self.render_valid = Some(render_valid);
        Ok(self)
    }

    pub fn values(&self) -> &[u8] {
        &self.values
    }

    pub fn render_valid_mask(&self) -> Option<&[u8]> {
        self.render_valid.as_deref()
    }

    pub fn geometric_voxel_count(&self) -> u64 {
        self.values.len() as u64
    }

    pub fn render_valid_voxel_count(&self) -> u64 {
        self.render_valid
            .as_ref()
            .map(|mask| mask.iter().filter(|value| **value == 1).count() as u64)
            .unwrap_or(self.geometric_voxel_count())
    }

    pub fn voxel(&self, z: u64, y: u64, x: u64) -> Option<u8> {
        if z >= self.shape.z || y >= self.shape.y || x >= self.shape.x {
            return None;
        }
        let index = ((z * self.shape.y + y) * self.shape.x + x) as usize;
        self.values.get(index).copied()
    }

    pub fn is_render_valid(&self, z: u64, y: u64, x: u64) -> Option<bool> {
        if z >= self.shape.z || y >= self.shape.y || x >= self.shape.x {
            return None;
        }
        let index = ((z * self.shape.y + y) * self.shape.x + x) as usize;
        Some(
            self.render_valid
                .as_ref()
                .and_then(|mask| mask.get(index))
                .map(|value| *value == 1)
                .unwrap_or(true),
        )
    }

    pub fn render_voxel(&self, z: u64, y: u64, x: u64) -> Option<u8> {
        if self.is_render_valid(z, y, x)? {
            self.voxel(z, y, x)
        } else {
            None
        }
    }
}

impl DenseVolumeU16 {
    pub fn new(
        dataset_id: DatasetId,
        layer_id: LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        shape: Shape3D,
        grid_to_world: GridToWorld,
        values: Vec<u16>,
    ) -> Result<Self, DataError> {
        let expected = shape
            .element_count()
            .map_err(DataError::InvalidShape)
            .and_then(|count| {
                usize::try_from(count).map_err(|err| DataError::ReadFailed {
                    layer_id: layer_id.to_string(),
                    message: err.to_string(),
                })
            })?;
        if values.len() != expected {
            return Err(DataError::VolumeValueCountMismatch {
                layer_id: layer_id.to_string(),
                actual: values.len(),
                expected,
            });
        }
        Ok(Self {
            dataset_id,
            layer_id,
            scale_level,
            timepoint,
            shape,
            grid_to_world,
            values,
            render_valid: None,
        })
    }

    pub fn with_render_valid(mut self, render_valid: Vec<u8>) -> Result<Self, DataError> {
        validate_render_valid_len(&self.layer_id, Some(&render_valid), self.values.len())?;
        if render_valid.iter().any(|value| !matches!(value, 0 | 1)) {
            return Err(DataError::ReadFailed {
                layer_id: self.layer_id.to_string(),
                message: "render-valid mask contains values other than 0 or 1".to_owned(),
            });
        }
        self.render_valid = Some(render_valid);
        Ok(self)
    }

    pub fn values(&self) -> &[u16] {
        &self.values
    }

    pub fn render_valid_mask(&self) -> Option<&[u8]> {
        self.render_valid.as_deref()
    }

    pub fn geometric_voxel_count(&self) -> u64 {
        self.values.len() as u64
    }

    pub fn render_valid_voxel_count(&self) -> u64 {
        self.render_valid
            .as_ref()
            .map(|mask| mask.iter().filter(|value| **value == 1).count() as u64)
            .unwrap_or(self.geometric_voxel_count())
    }

    pub fn voxel(&self, z: u64, y: u64, x: u64) -> Option<u16> {
        if z >= self.shape.z || y >= self.shape.y || x >= self.shape.x {
            return None;
        }
        let index = ((z * self.shape.y + y) * self.shape.x + x) as usize;
        self.values.get(index).copied()
    }

    pub fn is_render_valid(&self, z: u64, y: u64, x: u64) -> Option<bool> {
        if z >= self.shape.z || y >= self.shape.y || x >= self.shape.x {
            return None;
        }
        let index = ((z * self.shape.y + y) * self.shape.x + x) as usize;
        Some(
            self.render_valid
                .as_ref()
                .and_then(|mask| mask.get(index))
                .map(|value| *value == 1)
                .unwrap_or(true),
        )
    }

    pub fn render_voxel(&self, z: u64, y: u64, x: u64) -> Option<u16> {
        if self.is_render_valid(z, y, x)? {
            self.voxel(z, y, x)
        } else {
            None
        }
    }
}

impl DenseVolumeF32 {
    pub fn new(
        dataset_id: DatasetId,
        layer_id: LayerId,
        scale_level: u32,
        timepoint: TimeIndex,
        shape: Shape3D,
        grid_to_world: GridToWorld,
        values: Vec<f32>,
    ) -> Result<Self, DataError> {
        let expected = shape
            .element_count()
            .map_err(DataError::InvalidShape)
            .and_then(|count| {
                usize::try_from(count).map_err(|err| DataError::ReadFailed {
                    layer_id: layer_id.to_string(),
                    message: err.to_string(),
                })
            })?;
        if values.len() != expected {
            return Err(DataError::VolumeValueCountMismatch {
                layer_id: layer_id.to_string(),
                actual: values.len(),
                expected,
            });
        }
        Ok(Self {
            dataset_id,
            layer_id,
            scale_level,
            timepoint,
            shape,
            grid_to_world,
            values,
            render_valid: None,
        })
    }

    pub fn with_render_valid(mut self, render_valid: Vec<u8>) -> Result<Self, DataError> {
        validate_render_valid_len(&self.layer_id, Some(&render_valid), self.values.len())?;
        if render_valid.iter().any(|value| !matches!(value, 0 | 1)) {
            return Err(DataError::ReadFailed {
                layer_id: self.layer_id.to_string(),
                message: "render-valid mask contains values other than 0 or 1".to_owned(),
            });
        }
        self.render_valid = Some(render_valid);
        Ok(self)
    }

    pub fn values(&self) -> &[f32] {
        &self.values
    }

    pub fn render_valid_mask(&self) -> Option<&[u8]> {
        self.render_valid.as_deref()
    }

    pub fn geometric_voxel_count(&self) -> u64 {
        self.values.len() as u64
    }

    pub fn render_valid_voxel_count(&self) -> u64 {
        self.render_valid
            .as_ref()
            .map(|mask| mask.iter().filter(|value| **value == 1).count() as u64)
            .unwrap_or(self.geometric_voxel_count())
    }

    pub fn voxel(&self, z: u64, y: u64, x: u64) -> Option<f32> {
        if z >= self.shape.z || y >= self.shape.y || x >= self.shape.x {
            return None;
        }
        let index = ((z * self.shape.y + y) * self.shape.x + x) as usize;
        self.values.get(index).copied()
    }

    pub fn is_render_valid(&self, z: u64, y: u64, x: u64) -> Option<bool> {
        if z >= self.shape.z || y >= self.shape.y || x >= self.shape.x {
            return None;
        }
        let index = ((z * self.shape.y + y) * self.shape.x + x) as usize;
        Some(
            self.render_valid
                .as_ref()
                .and_then(|mask| mask.get(index))
                .map(|value| *value == 1)
                .unwrap_or(true),
        )
    }

    pub fn render_voxel(&self, z: u64, y: u64, x: u64) -> Option<f32> {
        if self.is_render_valid(z, y, x)? {
            self.voxel(z, y, x)
        } else {
            None
        }
    }
}

impl SpatialBrickIndex {
    pub fn new(z: u64, y: u64, x: u64) -> Self {
        Self { z, y, x }
    }
}

impl VolumeBrickU8 {
    pub fn values(&self) -> &[u8] {
        self.volume.values()
    }

    pub fn voxel(&self, z: u64, y: u64, x: u64) -> Option<u8> {
        self.volume.voxel(z, y, x)
    }

    pub fn render_voxel(&self, z: u64, y: u64, x: u64) -> Option<u8> {
        self.volume.render_voxel(z, y, x)
    }
}

impl VolumeBrickU16 {
    pub fn values(&self) -> &[u16] {
        self.volume.values()
    }

    pub fn voxel(&self, z: u64, y: u64, x: u64) -> Option<u16> {
        self.volume.voxel(z, y, x)
    }

    pub fn render_voxel(&self, z: u64, y: u64, x: u64) -> Option<u16> {
        self.volume.render_voxel(z, y, x)
    }
}

impl VolumeBrickF32 {
    pub fn values(&self) -> &[f32] {
        self.volume.values()
    }

    pub fn voxel(&self, z: u64, y: u64, x: u64) -> Option<f32> {
        self.volume.voxel(z, y, x)
    }

    pub fn render_voxel(&self, z: u64, y: u64, x: u64) -> Option<f32> {
        self.volume.render_voxel(z, y, x)
    }
}

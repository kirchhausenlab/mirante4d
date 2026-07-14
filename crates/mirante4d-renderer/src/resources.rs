use mirante4d_dataset::DatasetResourceIdentity;
use mirante4d_domain::{GridToWorld, LogicalLayerKey, ScaleLevel, Shape3D, TimeIndex};

use crate::{CurrentLeaseVolume, RenderError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceRepresentation {
    BrickedU8Atlas,
    BrickedU16Atlas,
    BrickedF32Atlas,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransformKey {
    matrix4x4_row_major_bits: [u64; 16],
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BrickAtlasResourceKey {
    pub identity: DatasetResourceIdentity,
    pub layer: LogicalLayerKey,
    pub scale_level: ScaleLevel,
    pub timepoint: TimeIndex,
    pub volume_shape: Shape3D,
    pub resource_shape: Shape3D,
    pub resource_grid_shape: Shape3D,
    pub transform: TransformKey,
    pub representation: ResourceRepresentation,
}

impl TransformKey {
    pub fn from_grid_to_world(grid_to_world: GridToWorld) -> Self {
        let mut matrix4x4_row_major_bits = [0u64; 16];
        for (index, value) in grid_to_world.row_major().iter().copied().enumerate() {
            matrix4x4_row_major_bits[index] = canonical_f64_bits(value);
        }
        Self {
            matrix4x4_row_major_bits,
        }
    }
}

impl BrickAtlasResourceKey {
    pub fn from_lease_volume(
        volume: CurrentLeaseVolume<'_>,
        representation: ResourceRepresentation,
    ) -> Result<Self, RenderError> {
        let resident = volume.resident();
        if resident.is_empty() {
            return Err(RenderError::ResourceIdentityMismatch(
                "lease-backed atlas input is empty",
            ));
        }
        Ok(Self {
            identity: resident.identity(),
            layer: resident.layer(),
            scale_level: resident.scale(),
            timepoint: resident.timepoint(),
            volume_shape: volume.volume_shape(),
            resource_shape: volume.resource_shape(),
            resource_grid_shape: volume.resource_grid_shape(),
            transform: TransformKey::from_grid_to_world(volume.grid_to_world()),
            representation,
        })
    }
}

fn canonical_f64_bits(value: f64) -> u64 {
    if value == 0.0 {
        0.0f64.to_bits()
    } else {
        value.to_bits()
    }
}

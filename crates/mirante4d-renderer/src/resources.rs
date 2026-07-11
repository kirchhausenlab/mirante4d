use mirante4d_data::{DenseVolumeF32, DenseVolumeU16};
use mirante4d_dataset::DatasetResourceIdentity;
use mirante4d_domain::{GridToWorld, LogicalLayerKey, ScaleLevel, Shape3D, TimeIndex};
use mirante4d_format::{DatasetId, LayerId};

use crate::{CurrentLeaseVolume, RenderError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ResourceGeneration(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceAuthority {
    DataEngine,
    AnalysisArtifact,
    SceneLayerState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderResourceKind {
    DenseIntensityVolume,
    BrickedIntensityAtlas,
    TrackLayer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResourceRepresentation {
    DenseU16,
    DenseF32,
    BrickedU8Atlas,
    BrickedU16Atlas,
    BrickedF32Atlas,
    TrackPolyline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransformKey {
    matrix4x4_row_major_bits: [u64; 16],
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DenseVolumeResourceKey {
    pub dataset_id: DatasetId,
    pub layer_id: LayerId,
    pub scale_level: ScaleLevel,
    pub timepoint: TimeIndex,
    pub shape: Shape3D,
    pub value_count: u64,
    pub transform: TransformKey,
    pub representation: ResourceRepresentation,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackLayerResourceId(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimeRangeKey {
    pub start: TimeIndex,
    pub end_exclusive: TimeIndex,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackLayerResourceKey {
    pub dataset_id: DatasetId,
    pub track_layer_id: TrackLayerResourceId,
    pub time_range: Option<TimeRangeKey>,
    pub representation: ResourceRepresentation,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RenderResourceKey {
    DenseVolume(DenseVolumeResourceKey),
    BrickAtlas(BrickAtlasResourceKey),
    TrackLayer(TrackLayerResourceKey),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RendererResourceHandle<K> {
    pub key: K,
    pub generation: ResourceGeneration,
    pub authority: ResourceAuthority,
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

impl DenseVolumeResourceKey {
    pub fn from_volume(volume: &DenseVolumeU16) -> Result<Self, RenderError> {
        let value_count = volume.values().len() as u64;
        let expected_count = volume.shape.element_count()?;
        if value_count != expected_count {
            return Err(RenderError::ResourceIdentityMismatch(
                "dense volume value count does not match shape",
            ));
        }

        Ok(Self {
            dataset_id: volume.dataset_id.clone(),
            layer_id: volume.layer_id.clone(),
            scale_level: ScaleLevel::new(volume.scale_level),
            timepoint: volume.timepoint,
            shape: volume.shape,
            value_count,
            transform: TransformKey::from_grid_to_world(volume.grid_to_world),
            representation: ResourceRepresentation::DenseU16,
        })
    }

    pub fn from_f32_volume(volume: &DenseVolumeF32) -> Result<Self, RenderError> {
        let value_count = volume.values().len() as u64;
        let expected_count = volume.shape.element_count()?;
        if value_count != expected_count {
            return Err(RenderError::ResourceIdentityMismatch(
                "dense f32 volume value count does not match shape",
            ));
        }

        Ok(Self {
            dataset_id: volume.dataset_id.clone(),
            layer_id: volume.layer_id.clone(),
            scale_level: ScaleLevel::new(volume.scale_level),
            timepoint: volume.timepoint,
            shape: volume.shape,
            value_count,
            transform: TransformKey::from_grid_to_world(volume.grid_to_world),
            representation: ResourceRepresentation::DenseF32,
        })
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

impl TrackLayerResourceId {
    pub fn new(value: impl Into<String>) -> Result<Self, RenderError> {
        let value = value.into();
        validate_resource_id("track layer", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<K> RendererResourceHandle<K> {
    pub fn new(key: K, generation: ResourceGeneration, authority: ResourceAuthority) -> Self {
        Self {
            key,
            generation,
            authority,
        }
    }
}

fn canonical_f64_bits(value: f64) -> u64 {
    if value == 0.0 {
        0.0f64.to_bits()
    } else {
        value.to_bits()
    }
}

fn validate_resource_id(kind: &'static str, value: &str) -> Result<(), RenderError> {
    if value.is_empty() {
        return Err(RenderError::InvalidResourceId {
            kind,
            value: value.to_owned(),
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(RenderError::InvalidResourceId {
            kind,
            value: value.to_owned(),
        });
    }
    Ok(())
}

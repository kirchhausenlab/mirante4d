use mirante4d_data::{DenseVolumeF32, DenseVolumeU16};
use mirante4d_domain::{GridToWorld, ScaleLevel, Shape3D, TimeIndex};
use mirante4d_format::{DatasetId, LayerId};

use crate::{
    RenderError,
    brick_render::{ResidentBrickSetF32, ResidentBrickSetU8, ResidentBrickSetU16},
};

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
    pub dataset_id: DatasetId,
    pub layer_id: LayerId,
    pub scale_level: ScaleLevel,
    pub timepoint: TimeIndex,
    pub volume_shape: Shape3D,
    pub brick_shape: Shape3D,
    pub brick_grid_shape: Shape3D,
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
    #[allow(clippy::too_many_arguments)]
    pub fn from_identity(
        dataset_id: DatasetId,
        layer_id: LayerId,
        scale_level: ScaleLevel,
        timepoint: TimeIndex,
        volume_shape: Shape3D,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        grid_to_world: GridToWorld,
        representation: ResourceRepresentation,
    ) -> Self {
        Self {
            dataset_id,
            layer_id,
            scale_level,
            timepoint,
            volume_shape,
            brick_shape,
            brick_grid_shape,
            transform: TransformKey::from_grid_to_world(grid_to_world),
            representation,
        }
    }

    pub fn from_resident_u8(
        resident: &ResidentBrickSetU8,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
    ) -> Result<Self, RenderError> {
        let first_brick =
            resident
                .bricks()
                .first()
                .ok_or(RenderError::ResourceIdentityMismatch(
                    "resident brick set is empty",
                ))?;
        let dataset_id = first_brick.volume.dataset_id.clone();
        let scale_level = first_brick.scale_level;

        for brick in resident.bricks() {
            if brick.volume.dataset_id != dataset_id {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident bricks come from different datasets",
                ));
            }
            if brick.volume.layer_id != resident.layer_id {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident brick layer does not match resident set layer",
                ));
            }
            if brick.volume.timepoint != resident.timepoint {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident brick timepoint does not match resident set timepoint",
                ));
            }
            if brick.scale_level != scale_level || brick.volume.scale_level != scale_level {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident bricks come from different scales",
                ));
            }
            if brick.values().len() as u64 != brick.volume.shape.element_count()? {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident brick value count does not match brick shape",
                ));
            }
        }

        Ok(Self {
            dataset_id,
            layer_id: resident.layer_id.clone(),
            scale_level: ScaleLevel::new(scale_level),
            timepoint: resident.timepoint,
            volume_shape: resident.volume_shape,
            brick_shape,
            brick_grid_shape,
            transform: TransformKey::from_grid_to_world(resident.grid_to_world),
            representation: ResourceRepresentation::BrickedU8Atlas,
        })
    }

    pub fn from_resident(
        resident: &ResidentBrickSetU16,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
    ) -> Result<Self, RenderError> {
        let first_brick =
            resident
                .bricks()
                .first()
                .ok_or(RenderError::ResourceIdentityMismatch(
                    "resident brick set is empty",
                ))?;
        let dataset_id = first_brick.volume.dataset_id.clone();
        let scale_level = first_brick.scale_level;

        for brick in resident.bricks() {
            if brick.volume.dataset_id != dataset_id {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident bricks come from different datasets",
                ));
            }
            if brick.volume.layer_id != resident.layer_id {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident brick layer does not match resident set layer",
                ));
            }
            if brick.volume.timepoint != resident.timepoint {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident brick timepoint does not match resident set timepoint",
                ));
            }
            if brick.scale_level != scale_level || brick.volume.scale_level != scale_level {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident bricks come from different scales",
                ));
            }
            if brick.values().len() as u64 != brick.volume.shape.element_count()? {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident brick value count does not match brick shape",
                ));
            }
        }

        Ok(Self {
            dataset_id,
            layer_id: resident.layer_id.clone(),
            scale_level: ScaleLevel::new(scale_level),
            timepoint: resident.timepoint,
            volume_shape: resident.volume_shape,
            brick_shape,
            brick_grid_shape,
            transform: TransformKey::from_grid_to_world(resident.grid_to_world),
            representation: ResourceRepresentation::BrickedU16Atlas,
        })
    }

    pub fn from_resident_f32(
        resident: &ResidentBrickSetF32,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
    ) -> Result<Self, RenderError> {
        let first_brick =
            resident
                .bricks()
                .first()
                .ok_or(RenderError::ResourceIdentityMismatch(
                    "resident brick set is empty",
                ))?;
        let dataset_id = first_brick.volume.dataset_id.clone();
        let scale_level = first_brick.scale_level;

        for brick in resident.bricks() {
            if brick.volume.dataset_id != dataset_id {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident bricks come from different datasets",
                ));
            }
            if brick.volume.layer_id != resident.layer_id {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident brick layer does not match resident set layer",
                ));
            }
            if brick.volume.timepoint != resident.timepoint {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident brick timepoint does not match resident set timepoint",
                ));
            }
            if brick.scale_level != scale_level || brick.volume.scale_level != scale_level {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident bricks come from different scales",
                ));
            }
            if brick.values().len() as u64 != brick.volume.shape.element_count()? {
                return Err(RenderError::ResourceIdentityMismatch(
                    "resident brick value count does not match brick shape",
                ));
            }
        }

        Ok(Self {
            dataset_id,
            layer_id: resident.layer_id.clone(),
            scale_level: ScaleLevel::new(scale_level),
            timepoint: resident.timepoint,
            volume_shape: resident.volume_shape,
            brick_shape,
            brick_grid_shape,
            transform: TransformKey::from_grid_to_world(resident.grid_to_world),
            representation: ResourceRepresentation::BrickedF32Atlas,
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

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use mirante4d_data::{
        DatasetHandle, DenseVolumeU8, SpatialBrickIndex, VolumeBrickU8, VolumeRegion,
    };
    use mirante4d_domain::{GridToWorld, Shape3D, Shape4D, TimeIndex};
    use mirante4d_format::{
        ChannelMetadata, DatasetId, DenseF32Layer, DenseU16Layer, ExistingPackagePolicy,
        FixtureKind, LayerId, NativeF32Dataset, NativeU16Dataset, WorldSpace, WorldUnit,
        default_f32_display, default_u16_display, write_fixture, write_native_f32_dataset,
        write_native_u16_dataset,
    };

    use super::*;

    #[test]
    fn dense_volume_key_includes_dataset_identity() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let dataset = DatasetHandle::open(&root).unwrap();
        let layer_id = dataset.first_layer_id().unwrap();
        let volume = dataset
            .read_u16_volume(&layer_id, TimeIndex::new(0))
            .unwrap();

        let mut other_dataset_volume = volume.clone();
        other_dataset_volume.dataset_id = DatasetId::new("other-dataset").unwrap();

        assert_ne!(
            DenseVolumeResourceKey::from_volume(&volume).unwrap(),
            DenseVolumeResourceKey::from_volume(&other_dataset_volume).unwrap()
        );
    }

    #[test]
    fn dense_volume_key_includes_transform_identity() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
        let dataset = DatasetHandle::open(&root).unwrap();
        let layer_id = dataset.first_layer_id().unwrap();
        let volume = dataset
            .read_u16_volume(&layer_id, TimeIndex::new(0))
            .unwrap();

        let mut shifted_volume = volume.clone();
        shifted_volume.grid_to_world = mirante4d_format::grid_to_world_scale_um(0.5, 0.5, 1.5);

        assert_ne!(
            DenseVolumeResourceKey::from_volume(&volume).unwrap(),
            DenseVolumeResourceKey::from_volume(&shifted_volume).unwrap()
        );
    }

    #[test]
    fn float32_dense_volume_key_uses_float32_representation() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_two_brick_f32_fixture(tempdir.path());
        let dataset = DatasetHandle::open(&root).unwrap();
        let layer_id = dataset.first_layer_id().unwrap();
        let volume = dataset
            .read_f32_volume(&layer_id, TimeIndex::new(0))
            .unwrap();

        let key = DenseVolumeResourceKey::from_f32_volume(&volume).unwrap();

        assert_eq!(key.representation, ResourceRepresentation::DenseF32);
        assert_eq!(key.value_count, volume.values().len() as u64);
    }

    #[test]
    fn brick_atlas_key_is_independent_of_current_resident_pages() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_two_brick_fixture(tempdir.path());
        let dataset = DatasetHandle::open(&root).unwrap();
        let layer_id = dataset.first_layer_id().unwrap();
        let brick_shape = dataset.brick_shape(&layer_id).unwrap();
        let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
        let volume_shape = dataset.scale_shape(&layer_id, 0).unwrap();
        let grid_to_world = dataset.scale_grid_to_world(&layer_id, 0).unwrap();
        let left = dataset
            .read_u16_brick(
                &layer_id,
                TimeIndex::new(0),
                SpatialBrickIndex::new(0, 0, 0),
            )
            .unwrap();
        let right = dataset
            .read_u16_brick(
                &layer_id,
                TimeIndex::new(0),
                SpatialBrickIndex::new(0, 0, 1),
            )
            .unwrap();

        let left_only = ResidentBrickSetU16::new(
            layer_id.clone(),
            TimeIndex::new(0),
            volume_shape,
            grid_to_world,
            vec![left.clone()],
        );
        let right_only = ResidentBrickSetU16::new(
            layer_id,
            TimeIndex::new(0),
            volume_shape,
            grid_to_world,
            vec![right],
        );

        assert_eq!(
            BrickAtlasResourceKey::from_resident(&left_only, brick_shape, brick_grid_shape)
                .unwrap(),
            BrickAtlasResourceKey::from_resident(&right_only, brick_shape, brick_grid_shape)
                .unwrap()
        );
    }

    #[test]
    fn float32_brick_atlas_key_uses_float32_representation() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_two_brick_f32_fixture(tempdir.path());
        let dataset = DatasetHandle::open(&root).unwrap();
        let layer_id = dataset.first_layer_id().unwrap();
        let brick_shape = dataset.brick_shape(&layer_id).unwrap();
        let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
        let volume_shape = dataset.scale_shape(&layer_id, 0).unwrap();
        let grid_to_world = dataset.scale_grid_to_world(&layer_id, 0).unwrap();
        let left = dataset
            .read_f32_brick(
                &layer_id,
                TimeIndex::new(0),
                SpatialBrickIndex::new(0, 0, 0),
            )
            .unwrap();
        let right = dataset
            .read_f32_brick(
                &layer_id,
                TimeIndex::new(0),
                SpatialBrickIndex::new(0, 0, 1),
            )
            .unwrap();

        let left_only = ResidentBrickSetF32::new(
            layer_id.clone(),
            TimeIndex::new(0),
            volume_shape,
            grid_to_world,
            vec![left],
        );
        let right_only = ResidentBrickSetF32::new(
            layer_id,
            TimeIndex::new(0),
            volume_shape,
            grid_to_world,
            vec![right],
        );

        let left_key =
            BrickAtlasResourceKey::from_resident_f32(&left_only, brick_shape, brick_grid_shape)
                .unwrap();
        let right_key =
            BrickAtlasResourceKey::from_resident_f32(&right_only, brick_shape, brick_grid_shape)
                .unwrap();

        assert_eq!(left_key, right_key);
        assert_eq!(
            left_key.representation,
            ResourceRepresentation::BrickedF32Atlas
        );
    }

    #[test]
    fn uint8_brick_atlas_key_uses_uint8_representation() {
        let layer_id = LayerId::new("ch0").unwrap();
        let volume = DenseVolumeU8::new(
            DatasetId::new("u8-resource").unwrap(),
            layer_id.clone(),
            0,
            TimeIndex::new(0),
            Shape3D::new(1, 1, 2).unwrap(),
            GridToWorld::identity(),
            vec![1, 2],
        )
        .unwrap();
        let brick = VolumeBrickU8 {
            scale_level: 0,
            brick_index: SpatialBrickIndex::new(0, 0, 0),
            chunk_index: mirante4d_format::BrickIndex {
                t: 0,
                z: 0,
                y: 0,
                x: 0,
            },
            region: VolumeRegion::new(0, 0, 0, 1, 1, 2).unwrap(),
            occupied: true,
            valid_voxel_count: 2,
            min: 1.0,
            max: 2.0,
            volume,
        };
        let resident = ResidentBrickSetU8::new(
            layer_id,
            TimeIndex::new(0),
            Shape3D::new(1, 1, 2).unwrap(),
            GridToWorld::identity(),
            vec![brick],
        );

        let key = BrickAtlasResourceKey::from_resident_u8(
            &resident,
            Shape3D::new(1, 1, 2).unwrap(),
            Shape3D::new(1, 1, 1).unwrap(),
        )
        .unwrap();

        assert_eq!(key.representation, ResourceRepresentation::BrickedU8Atlas);
    }

    #[test]
    fn brick_atlas_key_rejects_mixed_dataset_bricks() {
        let tempdir = tempfile::tempdir().unwrap();
        let root = write_two_brick_fixture(tempdir.path());
        let dataset = DatasetHandle::open(&root).unwrap();
        let layer_id = dataset.first_layer_id().unwrap();
        let brick_shape = dataset.brick_shape(&layer_id).unwrap();
        let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
        let volume_shape = dataset.scale_shape(&layer_id, 0).unwrap();
        let grid_to_world = dataset.scale_grid_to_world(&layer_id, 0).unwrap();
        let first = dataset
            .read_u16_brick(
                &layer_id,
                TimeIndex::new(0),
                SpatialBrickIndex::new(0, 0, 0),
            )
            .unwrap();
        let mut second = dataset
            .read_u16_brick(
                &layer_id,
                TimeIndex::new(0),
                SpatialBrickIndex::new(0, 0, 1),
            )
            .unwrap();
        second.volume.dataset_id = DatasetId::new("other-dataset").unwrap();
        let mixed = ResidentBrickSetU16::new(
            layer_id,
            TimeIndex::new(0),
            volume_shape,
            grid_to_world,
            vec![first, second],
        );

        assert_eq!(
            BrickAtlasResourceKey::from_resident(&mixed, brick_shape, brick_grid_shape)
                .unwrap_err(),
            RenderError::ResourceIdentityMismatch("resident bricks come from different datasets")
        );
    }

    #[test]
    fn resource_ids_are_strict_ascii_identifiers() {
        assert_eq!(
            TrackLayerResourceId::new("tracks_01").unwrap().as_str(),
            "tracks_01"
        );
        assert!(TrackLayerResourceId::new("track µ").is_err());
        assert!(TrackLayerResourceId::new("").is_err());
    }

    #[test]
    fn renderer_resource_handle_keeps_authority_explicit() {
        let key = TrackLayerResourceKey {
            dataset_id: DatasetId::new("dataset").unwrap(),
            track_layer_id: TrackLayerResourceId::new("tracks").unwrap(),
            time_range: Some(TimeRangeKey {
                start: TimeIndex::new(2),
                end_exclusive: TimeIndex::new(7),
            }),
            representation: ResourceRepresentation::TrackPolyline,
        };

        let handle = RendererResourceHandle::new(
            key.clone(),
            ResourceGeneration(3),
            ResourceAuthority::SceneLayerState,
        );

        assert_eq!(handle.key, key);
        assert_eq!(handle.generation, ResourceGeneration(3));
        assert_eq!(handle.authority, ResourceAuthority::SceneLayerState);
    }

    fn write_two_brick_fixture(root_parent: &Path) -> PathBuf {
        let root = root_parent.join("two-brick.m4d");
        write_native_u16_dataset(
            &root,
            NativeU16Dataset {
                id: "two-brick".to_owned(),
                name: "Two brick resource fixture".to_owned(),
                world_space: WorldSpace {
                    name: "sample".to_owned(),
                    unit: WorldUnit::Micrometer,
                },
                layers: vec![DenseU16Layer {
                    id: "ch0".to_owned(),
                    name: "channel 0".to_owned(),
                    channel: ChannelMetadata {
                        index: 0,
                        color_rgba: [0.0, 1.0, 0.0, 1.0],
                    },
                    shape: Shape4D::new(1, 2, 2, 4).unwrap(),
                    brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                    grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                    display: default_u16_display(),
                    values_tzyx: (0..16).collect(),
                }],
            },
            ExistingPackagePolicy::Fail,
        )
        .unwrap();
        root
    }

    fn write_two_brick_f32_fixture(root_parent: &Path) -> PathBuf {
        let root = root_parent.join("two-brick-f32.m4d");
        write_native_f32_dataset(
            &root,
            NativeF32Dataset {
                id: "two-brick-f32".to_owned(),
                name: "Two brick f32 resource fixture".to_owned(),
                world_space: WorldSpace {
                    name: "sample".to_owned(),
                    unit: WorldUnit::Micrometer,
                },
                layers: vec![DenseF32Layer {
                    id: "ch0".to_owned(),
                    name: "channel 0".to_owned(),
                    channel: ChannelMetadata {
                        index: 0,
                        color_rgba: [0.0, 1.0, 0.0, 1.0],
                    },
                    shape: Shape4D::new(1, 2, 2, 4).unwrap(),
                    brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                    grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                    display: default_f32_display(),
                    values_tzyx: (0..16).map(|value| value as f32 - 7.5).collect(),
                }],
            },
            ExistingPackagePolicy::Fail,
        )
        .unwrap();
        root
    }
}

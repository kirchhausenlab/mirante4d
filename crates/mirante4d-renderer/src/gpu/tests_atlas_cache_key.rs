use super::*;

use crate::resources::BrickAtlasResourceKey;

#[test]
fn brick_atlas_cache_key_is_independent_of_resident_pages() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = tempdir.path().join("two-brick.m4d");
    write_native_u16_dataset(
        &root,
        NativeU16Dataset {
            id: "two-brick".to_owned(),
            name: "Two brick cache-key fixture".to_owned(),
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
                grid_to_world: GridToWorld::scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: (0..16).collect(),
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();
    let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
    let grid_to_world = dataset.scale_grid_to_world(&layer_id, 0).unwrap();
    let left = dataset
        .read_u16_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 0))
        .unwrap();
    let right = dataset
        .read_u16_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 1))
        .unwrap();
    let left_first = ResidentBrickSetU16::new(
        layer_id.clone(),
        TimeIndex(0),
        Shape3D::new(2, 2, 4).unwrap(),
        grid_to_world,
        vec![left.clone(), right.clone()],
    );
    let right_first = ResidentBrickSetU16::new(
        layer_id,
        TimeIndex(0),
        Shape3D::new(2, 2, 4).unwrap(),
        grid_to_world,
        vec![right, left],
    );
    let left_only = ResidentBrickSetU16::new(
        left_first.layer_id.clone(),
        TimeIndex(0),
        Shape3D::new(2, 2, 4).unwrap(),
        left_first.grid_to_world,
        vec![left_first.bricks()[0].clone()],
    );
    let right_only = ResidentBrickSetU16::new(
        left_first.layer_id.clone(),
        TimeIndex(0),
        Shape3D::new(2, 2, 4).unwrap(),
        left_first.grid_to_world,
        vec![right_first.bricks()[0].clone()],
    );

    assert_eq!(
        BrickAtlasResourceKey::from_resident(&left_first, brick_shape, brick_grid_shape).unwrap(),
        BrickAtlasResourceKey::from_resident(&right_first, brick_shape, brick_grid_shape).unwrap()
    );
    assert_eq!(
        BrickAtlasResourceKey::from_resident(&left_only, brick_shape, brick_grid_shape).unwrap(),
        BrickAtlasResourceKey::from_resident(&right_only, brick_shape, brick_grid_shape).unwrap()
    );
}

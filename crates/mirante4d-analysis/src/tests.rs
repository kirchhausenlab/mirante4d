use approx::assert_abs_diff_eq;
use mirante4d_core::{GridToWorld, Shape4D, TimeIndex, WorldSpace, WorldUnit};
use mirante4d_data::DatasetHandle;
use mirante4d_format::{
    ChannelMetadata, DenseF32Layer, ExistingPackagePolicy, FixtureKind, NativeF32Dataset,
    default_f32_display, write_fixture, write_native_f32_dataset,
};

use super::*;

#[test]
fn summarizes_uint16_volume() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0)).unwrap();

    let summary = summarize_u16_volume(&volume);

    assert_eq!(summary.voxel_count, 4096);
    assert_eq!(summary.nonzero_count, 4095);
    assert_eq!(summary.min, 0);
    assert_eq!(summary.max, 4125);
    assert!(summary.mean > 0.0);
}

#[test]
fn summarizes_float32_volume_with_f64_accumulation() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = tempdir.path().join("analysis-f32.m4d");
    let values = vec![
        -1.0, 0.0, 0.5, 2.0, 4.25, -3.5, 8.0, -0.25, 1.25, 3.0, 5.5, 9.75,
    ];
    write_native_f32_dataset(
        &root,
        NativeF32Dataset {
            id: "analysis-f32".to_owned(),
            name: "Analysis F32".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            layers: vec![DenseF32Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [1.0, 1.0, 1.0, 1.0],
                },
                shape: Shape4D::new(1, 2, 2, 3).unwrap(),
                brick_shape: Shape4D::new(1, 1, 1, 3).unwrap(),
                grid_to_world: GridToWorld::scale_um(1.0, 1.0, 1.0),
                display: default_f32_display(),
                values_tzyx: values,
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_f32_volume(&layer_id, TimeIndex(0)).unwrap();

    let summary = summarize_f32_volume(&volume);

    assert_eq!(summary.voxel_count, 12);
    assert_eq!(summary.nonzero_count, 11);
    assert_eq!(summary.min, -3.5);
    assert_eq!(summary.max, 9.75);
    assert_abs_diff_eq!(summary.sum, 29.5, epsilon = 1e-12);
    assert_abs_diff_eq!(summary.mean, 29.5 / 12.0, epsilon = 1e-12);
}

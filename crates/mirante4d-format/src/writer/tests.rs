use mirante4d_core::{ChannelColor, DisplayWindow, LayerDisplay};

use super::*;
use crate::{
    NoDataPolicyKind, NoDataVisibilityPolicy,
    validate::{load_and_validate_dataset, load_manifest, validate_manifest, write_manifest},
    zarr_io::open_array,
};

fn array_zarr_metadata(package: &Path, array_path: &str) -> serde_json::Value {
    let metadata_path = package.join(array_path).join("zarr.json");
    let metadata_text = std::fs::read_to_string(metadata_path).unwrap();
    serde_json::from_str(&metadata_text).unwrap()
}

fn assert_dense_array_uses_zstd_level3(package: &Path, array_path: &str) {
    let metadata = array_zarr_metadata(package, array_path);
    let codecs = metadata
        .get("codecs")
        .and_then(serde_json::Value::as_array)
        .unwrap();
    assert_eq!(codecs.len(), 1);
    assert_eq!(codecs[0].get("name").unwrap(), "sharding_indexed");
    let inner_codecs = codecs[0]
        .get("configuration")
        .and_then(|configuration| configuration.get("codecs"))
        .and_then(serde_json::Value::as_array)
        .unwrap();
    assert_eq!(inner_codecs.len(), 2);
    assert_eq!(inner_codecs[0].get("name").unwrap(), "bytes");
    assert_eq!(
        inner_codecs[0]
            .get("configuration")
            .and_then(|configuration| configuration.get("endian"))
            .unwrap(),
        "little"
    );
    assert_eq!(inner_codecs[1].get("name").unwrap(), "zstd");
    assert_eq!(
        inner_codecs[1]
            .get("configuration")
            .and_then(|configuration| configuration.get("level"))
            .unwrap(),
        DENSE_INTENSITY_ZSTD_LEVEL
    );
    assert_eq!(
        inner_codecs[1]
            .get("configuration")
            .and_then(|configuration| configuration.get("checksum"))
            .unwrap(),
        false
    );
}

#[test]
fn validator_rejects_invalid_native_provenance() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("invalid-provenance.m4d");
    write_native_u16_dataset(
        &package,
        NativeU16Dataset {
            id: "invalid-provenance".to_owned(),
            name: "Invalid Provenance".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_core::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [1.0, 1.0, 1.0, 1.0],
                },
                shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world: GridToWorld::scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: vec![1, 2],
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let mut manifest = load_manifest(&package).unwrap();
    manifest.provenance.native_schema_version = 999;

    assert!(matches!(
        validate_manifest(&package, &manifest),
        Err(FormatError::InvalidProvenance(message))
            if message.contains("native_schema_version")
    ));
}

#[test]
fn streaming_layer_writer_writes_valid_timepoint_subsets() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("streaming.m4d");
    let shape = Shape4D::new(2, 2, 2, 3).unwrap();
    let grid_to_world = GridToWorld::scale_um(0.2, 0.3, 0.5);
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "streaming-test".to_owned(),
        "Streaming Test".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: mirante4d_core::WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let mut layer = writer
        .begin_streaming_layer(StreamingU16LayerSpec {
            id: "ch0".to_owned(),
            name: "Channel 0".to_owned(),
            channel: ChannelMetadata {
                index: 0,
                color_rgba: ChannelColor::new([0.0, 1.0, 0.0, 1.0]).unwrap().color_rgba,
            },
            source_dtype: IntensityDType::Uint16,
            shape,
            grid_to_world,
            display: LayerDisplay::new(true, DisplayWindow::new(0.0, 255.0).unwrap(), 1.0).unwrap(),
            scales: vec![StreamingU16ScaleSpec {
                level: 0,
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world,
                source_scale: None,
                reduction: ScaleReduction::Source,
            }],
        })
        .unwrap();

    let timepoint_0 = (0..12).collect::<Vec<u16>>();
    let timepoint_1 = (100..112).collect::<Vec<u16>>();
    layer.write_timepoint(0, 1, &timepoint_1).unwrap();
    layer.write_timepoint(0, 0, &timepoint_0).unwrap();
    assert_eq!(layer.scale_statistics(0).unwrap().min, 0.0);
    assert_eq!(layer.scale_statistics(0).unwrap().max, 111.0);
    writer.finish_streaming_layer(layer).unwrap();
    writer.finish().unwrap();

    let manifest = load_manifest(&package).unwrap();
    let scale = &manifest.layers[0].scales[0];
    assert_eq!(scale.bricks.grid_shape, Shape4D::new(2, 2, 2, 2).unwrap());
    assert_eq!(
        scale.bricks.records.len(),
        scale.bricks.grid_shape.element_count().unwrap() as usize
    );
    assert_eq!(scale.statistics.min, 0.0);
    assert_eq!(scale.statistics.max, 111.0);
    assert_dense_array_uses_zstd_level3(&package, "arrays/intensity/ch0/s0");
    assert_eq!(scale.bricks.range_hierarchy.levels.len(), 2);
    assert_eq!(
        scale.bricks.range_hierarchy.levels[0].grid_shape,
        scale.bricks.grid_shape
    );
    assert_eq!(
        scale.bricks.range_hierarchy.levels[1].grid_shape,
        Shape4D::new(2, 1, 1, 1).unwrap()
    );
    assert_eq!(scale.bricks.range_hierarchy.levels[1].records[0].min, 0.0);
    assert_eq!(scale.bricks.range_hierarchy.levels[1].records[0].max, 11.0);
    assert_eq!(
        scale.bricks.range_hierarchy.levels[1].records[0].valid_voxel_count,
        12
    );
    assert_eq!(scale.bricks.range_hierarchy.levels[1].records[1].min, 100.0);
    assert_eq!(scale.bricks.range_hierarchy.levels[1].records[1].max, 111.0);
    assert_eq!(
        scale.bricks.range_hierarchy.levels[1].records[1].valid_voxel_count,
        12
    );

    let array = open_array(&package, "arrays/intensity/ch0/s0").unwrap();
    let decoded: Vec<u16> = array
        .retrieve_array_subset(&[1..2, 0..2, 0..2, 0..3])
        .unwrap();
    assert_eq!(decoded, timepoint_1);
}

#[test]
fn streaming_u8_layer_writer_preserves_uint8_storage_and_metadata() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("streaming-u8.m4d");
    let shape = Shape4D::new(2, 1, 2, 4).unwrap();
    let grid_to_world = GridToWorld::scale_um(0.2, 0.3, 0.5);
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "streaming-u8-test".to_owned(),
        "Streaming U8 Test".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: mirante4d_core::WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let mut layer = writer
        .begin_streaming_u8_layer(StreamingU8LayerSpec {
            id: "ch0".to_owned(),
            name: "Channel 0".to_owned(),
            channel: ChannelMetadata {
                index: 0,
                color_rgba: ChannelColor::new([0.0, 1.0, 0.0, 1.0]).unwrap().color_rgba,
            },
            shape,
            no_data_policy: None,
            grid_to_world,
            display: LayerDisplay::new(true, DisplayWindow::new(0.0, 255.0).unwrap(), 1.0).unwrap(),
            scales: vec![StreamingU8ScaleSpec {
                level: 0,
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world,
                source_scale: None,
                reduction: ScaleReduction::Source,
            }],
        })
        .unwrap();

    let timepoint_0 = (0..8).collect::<Vec<u8>>();
    let timepoint_1 = (100..108).collect::<Vec<u8>>();
    layer.write_timepoint(0, 0, &timepoint_0).unwrap();
    layer.write_timepoint(0, 1, &timepoint_1).unwrap();
    assert_eq!(layer.scale_statistics(0).unwrap().min, 0.0);
    assert_eq!(layer.scale_statistics(0).unwrap().max, 107.0);
    writer.finish_streaming_u8_layer(layer).unwrap();
    writer.finish().unwrap();

    let manifest = load_manifest(&package).unwrap();
    assert_eq!(manifest.layers[0].dtype.source, IntensityDType::Uint8);
    assert_eq!(manifest.layers[0].dtype.stored, IntensityDType::Uint8);
    assert_eq!(
        manifest.layers[0].scales[0].statistics.histogram.range_max,
        255.0
    );
    assert!(
        manifest.layers[0].scales[0]
            .storage
            .shard_records
            .iter()
            .all(|record| record.payload_bytes > 0)
    );
    assert_dense_array_uses_zstd_level3(&package, "arrays/intensity/ch0/s0");

    let array = open_array(&package, "arrays/intensity/ch0/s0").unwrap();
    let decoded: Vec<u8> = array
        .retrieve_array_subset(&[1..2, 0..1, 0..2, 0..4])
        .unwrap();
    assert_eq!(decoded, timepoint_1);
}

#[test]
fn validator_rejects_uint8_no_data_value_outside_source_range() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("invalid-nodata.m4d");
    let shape = Shape4D::new(1, 1, 1, 2).unwrap();
    let grid_to_world = GridToWorld::identity();
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "invalid-nodata".to_owned(),
        "Invalid No Data".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: mirante4d_core::WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    let mut layer = writer
        .begin_streaming_u8_layer(StreamingU8LayerSpec {
            id: "ch0".to_owned(),
            name: "Channel 0".to_owned(),
            channel: ChannelMetadata {
                index: 0,
                color_rgba: [1.0, 1.0, 1.0, 1.0],
            },
            shape,
            no_data_policy: None,
            grid_to_world,
            display: LayerDisplay::new(true, DisplayWindow::new(0.0, 255.0).unwrap(), 1.0).unwrap(),
            scales: vec![StreamingU8ScaleSpec {
                level: 0,
                shape,
                brick_shape: shape,
                grid_to_world,
                source_scale: None,
                reduction: ScaleReduction::Source,
            }],
        })
        .unwrap();
    layer.write_timepoint(0, 0, &[1, 2]).unwrap();
    writer.finish_streaming_u8_layer(layer).unwrap();
    writer.finish().unwrap();

    let mut manifest = load_manifest(&package).unwrap();
    manifest.layers[0].no_data_policy = Some(NoDataPolicy {
        kind: NoDataPolicyKind::SentinelValue,
        source_value: 256.0,
        source_dtype: IntensityDType::Uint8,
        visibility_policy: NoDataVisibilityPolicy::InvisibleWith1VoxelInvalidDilation,
    });

    assert!(matches!(
        validate_manifest(&package, &manifest),
        Err(FormatError::InvalidNoDataPolicy { layer_id, .. }) if layer_id == "ch0"
    ));
}

#[test]
fn streaming_u8_no_data_marks_all_invalid_brick_unoccupied() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("all-invalid-nodata.m4d");
    let shape = Shape4D::new(1, 1, 1, 2).unwrap();
    let grid_to_world = GridToWorld::identity();
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "all-invalid-nodata".to_owned(),
        "All Invalid No Data".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: mirante4d_core::WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    let mut layer = writer
        .begin_streaming_u8_layer(StreamingU8LayerSpec {
            id: "ch0".to_owned(),
            name: "Channel 0".to_owned(),
            channel: ChannelMetadata {
                index: 0,
                color_rgba: [1.0, 1.0, 1.0, 1.0],
            },
            shape,
            no_data_policy: Some(NoDataPolicy {
                kind: NoDataPolicyKind::SentinelValue,
                source_value: 255.0,
                source_dtype: IntensityDType::Uint8,
                visibility_policy: NoDataVisibilityPolicy::InvisibleWith1VoxelInvalidDilation,
            }),
            grid_to_world,
            display: LayerDisplay::new(true, DisplayWindow::new(0.0, 255.0).unwrap(), 1.0).unwrap(),
            scales: vec![StreamingU8ScaleSpec {
                level: 0,
                shape,
                brick_shape: shape,
                grid_to_world,
                source_scale: None,
                reduction: ScaleReduction::Source,
            }],
        })
        .unwrap();
    layer
        .write_timepoint_with_render_valid(0, 0, &[255, 255], &[0, 0])
        .unwrap();
    writer.finish_streaming_u8_layer(layer).unwrap();
    writer.finish().unwrap();

    let manifest = load_manifest(&package).unwrap();
    let scale = &manifest.layers[0].scales[0];
    let validity = scale.validity.as_ref().unwrap();
    assert_eq!(validity.valid_voxel_count, 0);
    assert_eq!(validity.invalid_voxel_count, 2);
    assert_eq!(scale.statistics.min, 0.0);
    assert_eq!(scale.statistics.max, 0.0);
    assert_eq!(scale.statistics.histogram.bins.iter().sum::<u64>(), 0);
    assert!(!scale.bricks.records[0].occupied);
    assert_eq!(scale.bricks.records[0].valid_voxel_count, 0);
}

#[test]
fn manifest_records_brick_shape_in_storage_not_legacy_chunk_shape() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("storage-brick-shape.m4d");
    let shape = Shape4D::new(1, 1, 1, 2).unwrap();
    let brick_shape = Shape4D::new(1, 1, 1, 1).unwrap();
    let grid_to_world = GridToWorld::identity();
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "storage-brick-shape".to_owned(),
        "Storage Brick Shape".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: mirante4d_core::WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    let mut layer = writer
        .begin_streaming_u8_layer(StreamingU8LayerSpec {
            id: "ch0".to_owned(),
            name: "Channel 0".to_owned(),
            channel: ChannelMetadata {
                index: 0,
                color_rgba: [1.0, 1.0, 1.0, 1.0],
            },
            shape,
            no_data_policy: Some(NoDataPolicy {
                kind: NoDataPolicyKind::SentinelValue,
                source_value: 255.0,
                source_dtype: IntensityDType::Uint8,
                visibility_policy: NoDataVisibilityPolicy::InvisibleWith1VoxelInvalidDilation,
            }),
            grid_to_world,
            display: LayerDisplay::new(true, DisplayWindow::new(0.0, 255.0).unwrap(), 1.0).unwrap(),
            scales: vec![StreamingU8ScaleSpec {
                level: 0,
                shape,
                brick_shape,
                grid_to_world,
                source_scale: None,
                reduction: ScaleReduction::Source,
            }],
        })
        .unwrap();
    layer
        .write_timepoint_with_render_valid(0, 0, &[1, 255], &[1, 0])
        .unwrap();
    writer.finish_streaming_u8_layer(layer).unwrap();
    writer.finish().unwrap();

    let manifest_text = std::fs::read_to_string(package.join("mirante4d.json")).unwrap();
    let manifest_json: serde_json::Value = serde_json::from_str(&manifest_text).unwrap();
    let scale = &manifest_json["layers"][0]["scales"][0];
    let shape_json = serde_json::json!({ "t": 1, "z": 1, "y": 1, "x": 1 });

    assert!(scale.get("chunk_shape").is_none());
    assert_eq!(scale["storage"]["brick_shape"], shape_json);
    let validity = &scale["validity"];
    assert!(validity.get("chunk_shape").is_none());
    assert_eq!(validity["storage"]["brick_shape"], shape_json);
}

#[test]
fn streaming_u8_layer_writer_writes_z_slabs_incrementally() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("streaming-u8-slabs.m4d");
    let shape = Shape4D::new(1, 3, 2, 2).unwrap();
    let grid_to_world = GridToWorld::scale_um(1.0, 1.0, 1.0);
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "streaming-u8-slabs".to_owned(),
        "Streaming U8 Slabs".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: mirante4d_core::WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let mut layer = writer
        .begin_streaming_u8_layer(StreamingU8LayerSpec {
            id: "ch0".to_owned(),
            name: "Channel 0".to_owned(),
            channel: ChannelMetadata {
                index: 0,
                color_rgba: ChannelColor::new([0.0, 1.0, 0.0, 1.0]).unwrap().color_rgba,
            },
            shape,
            no_data_policy: None,
            grid_to_world,
            display: LayerDisplay::new(true, DisplayWindow::new(0.0, 255.0).unwrap(), 1.0).unwrap(),
            scales: vec![StreamingU8ScaleSpec {
                level: 0,
                shape,
                brick_shape: Shape4D::new(1, 1, 2, 2).unwrap(),
                grid_to_world,
                source_scale: None,
                reduction: ScaleReduction::Source,
            }],
        })
        .unwrap();

    layer.write_z_slab(0, 0, 1, &[10, 11, 12, 13]).unwrap();
    let overlap = layer.write_z_slab(0, 0, 1, &[20, 21, 22, 23]).unwrap_err();
    assert!(matches!(
        overlap,
        FormatError::DuplicateTimepointWrite { .. }
    ));
    layer.write_z_slab(0, 0, 0, &[0, 1, 2, 3]).unwrap();
    layer.write_z_slab(0, 0, 2, &[20, 21, 22, 23]).unwrap();
    assert_eq!(layer.scale_statistics(0).unwrap().min, 0.0);
    assert_eq!(layer.scale_statistics(0).unwrap().max, 23.0);
    writer.finish_streaming_u8_layer(layer).unwrap();
    writer.finish().unwrap();

    let manifest = load_manifest(&package).unwrap();
    let scale = &manifest.layers[0].scales[0];
    assert_eq!(scale.statistics.min, 0.0);
    assert_eq!(scale.statistics.max, 23.0);
    assert_eq!(scale.bricks.records.len(), 3);
    assert!(
        scale
            .storage
            .shard_records
            .iter()
            .all(|record| record.payload_bytes > 0)
    );

    let array = open_array(&package, "arrays/intensity/ch0/s0").unwrap();
    let decoded: Vec<u8> = array
        .retrieve_array_subset(&[0..1, 0..3, 0..2, 0..2])
        .unwrap();
    assert_eq!(decoded, vec![0, 1, 2, 3, 10, 11, 12, 13, 20, 21, 22, 23]);
}

#[test]
fn streaming_u16_layer_writer_writes_z_slabs_incrementally() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("streaming-u16-slabs.m4d");
    let shape = Shape4D::new(1, 3, 2, 2).unwrap();
    let grid_to_world = GridToWorld::scale_um(1.0, 1.0, 1.0);
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "streaming-u16-slabs".to_owned(),
        "Streaming U16 Slabs".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: mirante4d_core::WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let mut layer = writer
        .begin_streaming_layer(StreamingU16LayerSpec {
            id: "ch0".to_owned(),
            name: "Channel 0".to_owned(),
            channel: ChannelMetadata {
                index: 0,
                color_rgba: ChannelColor::new([0.0, 1.0, 0.0, 1.0]).unwrap().color_rgba,
            },
            source_dtype: IntensityDType::Uint16,
            shape,
            grid_to_world,
            display: default_u16_display(),
            scales: vec![StreamingU16ScaleSpec {
                level: 0,
                shape,
                brick_shape: Shape4D::new(1, 1, 2, 2).unwrap(),
                grid_to_world,
                source_scale: None,
                reduction: ScaleReduction::Source,
            }],
        })
        .unwrap();

    layer.write_z_slab(0, 0, 0, &[0, 1, 2, 3]).unwrap();
    layer.write_z_slab(0, 0, 1, &[100, 101, 102, 103]).unwrap();
    layer.write_z_slab(0, 0, 2, &[200, 201, 202, 203]).unwrap();
    assert_eq!(layer.scale_statistics(0).unwrap().min, 0.0);
    assert_eq!(layer.scale_statistics(0).unwrap().max, 203.0);
    writer.finish_streaming_layer(layer).unwrap();
    writer.finish().unwrap();

    let manifest = load_manifest(&package).unwrap();
    assert_eq!(manifest.layers[0].dtype.source, IntensityDType::Uint16);
    assert_eq!(manifest.layers[0].dtype.stored, IntensityDType::Uint16);
    assert_eq!(manifest.layers[0].scales[0].statistics.max, 203.0);

    let array = open_array(&package, "arrays/intensity/ch0/s0").unwrap();
    let decoded: Vec<u16> = array
        .retrieve_array_subset(&[0..1, 0..3, 0..2, 0..2])
        .unwrap();
    assert_eq!(
        decoded,
        vec![0, 1, 2, 3, 100, 101, 102, 103, 200, 201, 202, 203]
    );
}

#[test]
fn streaming_f32_layer_writer_writes_z_slabs_incrementally() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("streaming-f32-slabs.m4d");
    let shape = Shape4D::new(1, 3, 2, 2).unwrap();
    let grid_to_world = GridToWorld::scale_um(1.0, 1.0, 1.0);
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "streaming-f32-slabs".to_owned(),
        "Streaming F32 Slabs".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: mirante4d_core::WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let mut layer = writer
        .begin_streaming_f32_layer(StreamingF32LayerSpec {
            id: "ch0".to_owned(),
            name: "Channel 0".to_owned(),
            channel: ChannelMetadata {
                index: 0,
                color_rgba: ChannelColor::new([1.0, 1.0, 1.0, 1.0]).unwrap().color_rgba,
            },
            shape,
            grid_to_world,
            display: default_f32_display(),
            scales: vec![StreamingF32ScaleSpec {
                level: 0,
                shape,
                brick_shape: Shape4D::new(1, 1, 2, 2).unwrap(),
                grid_to_world,
                source_scale: None,
                reduction: ScaleReduction::Source,
            }],
        })
        .unwrap();

    layer.write_z_slab(0, 0, 0, &[-1.0, 0.0, 0.5, 1.0]).unwrap();
    layer
        .write_z_slab(0, 0, 1, &[2.0, 3.25, 4.5, 5.75])
        .unwrap();
    layer
        .write_z_slab(0, 0, 2, &[6.0, 7.25, 8.5, 9.75])
        .unwrap();
    assert_eq!(layer.scale_statistics(0).unwrap().min, -1.0);
    assert_eq!(layer.scale_statistics(0).unwrap().max, 9.75);
    writer.finish_streaming_f32_layer(layer).unwrap();
    writer.finish().unwrap();

    let manifest = load_manifest(&package).unwrap();
    assert_eq!(manifest.layers[0].dtype.source, IntensityDType::Float32);
    assert_eq!(manifest.layers[0].dtype.stored, IntensityDType::Float32);
    assert_eq!(manifest.layers[0].scales[0].statistics.min, -1.0);

    let array = open_array(&package, "arrays/intensity/ch0/s0").unwrap();
    let decoded: Vec<f32> = array
        .retrieve_array_subset(&[0..1, 0..3, 0..2, 0..2])
        .unwrap();
    assert_eq!(
        decoded,
        vec![
            -1.0, 0.0, 0.5, 1.0, 2.0, 3.25, 4.5, 5.75, 6.0, 7.25, 8.5, 9.75
        ]
    );
}

#[test]
fn streaming_f32_layer_writer_preserves_float32_storage_and_metadata() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("streaming-f32.m4d");
    let shape = Shape4D::new(2, 1, 2, 4).unwrap();
    let grid_to_world = GridToWorld::scale_um(0.2, 0.3, 0.5);
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "streaming-f32-test".to_owned(),
        "Streaming F32 Test".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: mirante4d_core::WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let mut layer = writer
        .begin_streaming_f32_layer(StreamingF32LayerSpec {
            id: "ch0".to_owned(),
            name: "Channel 0".to_owned(),
            channel: ChannelMetadata {
                index: 0,
                color_rgba: ChannelColor::new([1.0, 1.0, 1.0, 1.0]).unwrap().color_rgba,
            },
            shape,
            grid_to_world,
            display: default_f32_display(),
            scales: vec![StreamingF32ScaleSpec {
                level: 0,
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world,
                source_scale: None,
                reduction: ScaleReduction::Source,
            }],
        })
        .unwrap();

    let timepoint_0 = vec![-1.0, 0.0, 0.5, 1.0, 2.25, 3.5, 4.75, 6.0];
    let timepoint_1 = vec![10.0, 0.0, -2.5, 12.25, 8.0, 9.5, 11.0, 13.75];
    layer.write_timepoint(0, 1, &timepoint_1).unwrap();
    layer.write_timepoint(0, 0, &timepoint_0).unwrap();
    let source_statistics = layer.scale_statistics(0).unwrap();
    assert_eq!(source_statistics.min, -2.5);
    assert_eq!(source_statistics.max, 13.75);
    assert_eq!(source_statistics.histogram.bin_count, 4096);
    assert_eq!(
        source_statistics.histogram.bins.iter().sum::<u64>(),
        (timepoint_0.len() + timepoint_1.len()) as u64
    );
    writer.finish_streaming_f32_layer(layer).unwrap();
    writer.finish().unwrap();

    let manifest = load_manifest(&package).unwrap();
    let layer = &manifest.layers[0];
    let scale = &layer.scales[0];
    assert_eq!(layer.dtype.source, IntensityDType::Float32);
    assert_eq!(layer.dtype.stored, IntensityDType::Float32);
    assert_eq!(scale.statistics.min, -2.5);
    assert_eq!(scale.statistics.max, 13.75);
    assert_eq!(scale.statistics.histogram.bin_count, 4096);
    assert_eq!(
        scale.statistics.histogram.bins.iter().sum::<u64>(),
        (timepoint_0.len() + timepoint_1.len()) as u64
    );
    assert_eq!(
        scale.bricks.records.len(),
        scale.bricks.grid_shape.element_count().unwrap() as usize
    );
    assert!(
        scale
            .storage
            .shard_records
            .iter()
            .all(|record| record.payload_bytes > 0)
    );
    assert_dense_array_uses_zstd_level3(&package, "arrays/intensity/ch0/s0");

    let array = open_array(&package, "arrays/intensity/ch0/s0").unwrap();
    let decoded: Vec<f32> = array
        .retrieve_array_subset(&[1..2, 0..1, 0..2, 0..4])
        .unwrap();
    assert_eq!(decoded, timepoint_1);
}

#[test]
fn streaming_f32_layer_writer_rejects_nonfinite_values() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("streaming-f32-nan.m4d");
    let shape = Shape4D::new(1, 1, 1, 2).unwrap();
    let grid_to_world = GridToWorld::scale_um(1.0, 1.0, 1.0);
    let writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "streaming-f32-nan".to_owned(),
        "Streaming F32 NaN".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: mirante4d_core::WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let mut layer = writer
        .begin_streaming_f32_layer(StreamingF32LayerSpec {
            id: "ch0".to_owned(),
            name: "Channel 0".to_owned(),
            channel: ChannelMetadata {
                index: 0,
                color_rgba: ChannelColor::new([1.0, 1.0, 1.0, 1.0]).unwrap().color_rgba,
            },
            shape,
            grid_to_world,
            display: default_f32_display(),
            scales: vec![StreamingF32ScaleSpec {
                level: 0,
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world,
                source_scale: None,
                reduction: ScaleReduction::Source,
            }],
        })
        .unwrap();

    let err = layer.write_timepoint(0, 0, &[0.0, f32::NAN]).unwrap_err();

    assert!(matches!(
        err,
        FormatError::InvalidFloatValue {
            layer_id,
            index: 1,
            value
        } if layer_id == "ch0" && value.is_nan()
    ));
}

#[test]
fn dense_f32_layer_writer_preserves_float32_storage_and_metadata() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("native-f32.m4d");
    let shape = Shape4D::new(1, 1, 2, 4).unwrap();
    let grid_to_world = GridToWorld::scale_um(0.2, 0.3, 0.5);
    let values = vec![0.0, 0.25, 1.5, -2.0, 10.0, 11.25, 12.5, 13.75];

    write_native_f32_dataset(
        &package,
        NativeF32Dataset {
            id: "native-f32".to_owned(),
            name: "Native F32".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_core::WorldUnit::Micrometer,
            },
            layers: vec![DenseF32Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [1.0, 1.0, 1.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world,
                display: default_f32_display(),
                values_tzyx: values.clone(),
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let manifest = load_manifest(&package).unwrap();
    let layer = &manifest.layers[0];
    let scale = &layer.scales[0];
    assert_eq!(layer.dtype.source, IntensityDType::Float32);
    assert_eq!(layer.dtype.stored, IntensityDType::Float32);
    assert_eq!(layer.dtype.conversion, DTypeConversion::Lossless);
    assert_eq!(scale.statistics.min, -2.0);
    assert_eq!(scale.statistics.max, 13.75);
    assert_eq!(scale.statistics.histogram.bin_count, 4096);
    assert_eq!(scale.statistics.histogram.range_min, -2.0);
    assert_eq!(scale.statistics.histogram.range_max, 13.75);
    assert_eq!(
        scale.statistics.histogram.bins.iter().sum::<u64>(),
        values.len() as u64
    );
    assert_eq!(
        scale.bricks.records.len(),
        scale.bricks.grid_shape.element_count().unwrap() as usize
    );
    assert!(
        scale
            .storage
            .shard_records
            .iter()
            .all(|record| record.payload_bytes > 0)
    );

    let array = open_array(&package, "arrays/intensity/ch0/s0").unwrap();
    let decoded: Vec<f32> = array
        .retrieve_array_subset(&[0..1, 0..1, 0..2, 0..4])
        .unwrap();
    assert_eq!(decoded, values);
}

#[test]
fn dense_f32_writer_rejects_nonfinite_values() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("native-f32-nan.m4d");
    let shape = Shape4D::new(1, 1, 1, 2).unwrap();

    let err = write_native_f32_dataset(
        &package,
        NativeF32Dataset {
            id: "native-f32-nan".to_owned(),
            name: "Native F32 NaN".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_core::WorldUnit::Micrometer,
            },
            layers: vec![DenseF32Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [1.0, 1.0, 1.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world: GridToWorld::scale_um(1.0, 1.0, 1.0),
                display: default_f32_display(),
                values_tzyx: vec![0.0, f32::NAN],
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap_err();

    assert!(matches!(
        err,
        FormatError::InvalidFloatValue {
            layer_id,
            index: 1,
            value,
        } if layer_id == "ch0" && value.is_nan()
    ));
}

#[test]
fn validator_rejects_unsupported_dense_array_codec_configuration() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("unsupported-codec.m4d");
    let shape = Shape4D::new(1, 1, 1, 2).unwrap();

    write_native_u16_dataset(
        &package,
        NativeU16Dataset {
            id: "unsupported-codec".to_owned(),
            name: "Unsupported Codec".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_core::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [1.0, 1.0, 1.0, 1.0],
                },
                shape,
                brick_shape: shape,
                grid_to_world: GridToWorld::scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: vec![1, 2],
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let metadata_path = package.join("arrays/intensity/ch0/s0/zarr.json");
    let mut metadata = array_zarr_metadata(&package, "arrays/intensity/ch0/s0");
    metadata["codecs"][0]["configuration"]["codecs"][1]["configuration"]["level"] =
        serde_json::json!(5);
    std::fs::write(
        &metadata_path,
        format!("{}\n", serde_json::to_string_pretty(&metadata).unwrap()),
    )
    .unwrap();

    let err = load_and_validate_dataset(&package).unwrap_err();
    assert!(matches!(
        err,
        FormatError::UnsupportedZarrCodec {
            layer_id,
            array_path,
            ..
        } if layer_id == "ch0" && array_path == "arrays/intensity/ch0/s0"
    ));
}

#[test]
fn validator_rejects_stale_brick_range_hierarchy() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("stale-range-hierarchy.m4d");
    let shape = Shape4D::new(1, 1, 1, 4).unwrap();

    write_native_u16_dataset(
        &package,
        NativeU16Dataset {
            id: "stale-range-hierarchy".to_owned(),
            name: "Stale Range Hierarchy".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_core::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [1.0, 1.0, 1.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world: GridToWorld::scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: vec![0, 0, 7, 0],
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    let mut manifest = load_manifest(&package).unwrap();
    manifest.layers[0].scales[0].bricks.range_hierarchy.levels[0].records[0].valid_voxel_count += 1;
    write_manifest(&package, &manifest).unwrap();

    let err = load_and_validate_dataset(&package).unwrap_err();

    assert!(matches!(
        err,
        FormatError::BrickRangeHierarchyMismatch { layer_id } if layer_id == "ch0"
    ));
}

#[test]
fn writer_rejects_stale_origin_multiscale_transform() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("stale-origin-scale.m4d");
    let s0_shape = Shape4D::new(1, 4, 4, 4).unwrap();
    let s1_shape = Shape4D::new(1, 2, 2, 2).unwrap();
    let s0_grid_to_world = GridToWorld::scale_um(0.2, 0.2, 0.2);
    let stale_s1_grid_to_world = GridToWorld::scale_um(0.4, 0.4, 0.4);

    let err = write_native_u16_multiscale_dataset(
        &package,
        NativeU16MultiscaleDataset {
            id: "stale-origin-scale".to_owned(),
            name: "Stale Origin Scale".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_core::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16MultiscaleLayer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [1.0, 1.0, 1.0, 1.0],
                },
                shape: s0_shape,
                grid_to_world: s0_grid_to_world,
                display: default_u16_display(),
                scales: vec![
                    DenseU16Scale {
                        level: 0,
                        shape: s0_shape,
                        brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                        grid_to_world: s0_grid_to_world,
                        source_scale: None,
                        reduction: ScaleReduction::Source,
                        values_tzyx: vec![1; s0_shape.element_count().unwrap() as usize],
                    },
                    DenseU16Scale {
                        level: 1,
                        shape: s1_shape,
                        brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                        grid_to_world: stale_s1_grid_to_world,
                        source_scale: Some(0),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: vec![1; s1_shape.element_count().unwrap() as usize],
                    },
                ],
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap_err();

    assert!(matches!(
        err,
        FormatError::ScaleTransformMismatch { layer_id, level }
            if layer_id == "ch0" && level == 1
    ));
}

#[test]
fn streaming_layer_writer_marks_zero_only_dense_bricks_valid() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("streaming-valid-zero.m4d");
    let shape = Shape4D::new(1, 1, 1, 4).unwrap();
    let grid_to_world = GridToWorld::scale_um(1.0, 1.0, 1.0);
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "streaming-valid-zero".to_owned(),
        "Streaming Valid Zero".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: mirante4d_core::WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let mut layer = writer
        .begin_streaming_layer(StreamingU16LayerSpec {
            id: "ch0".to_owned(),
            name: "Channel 0".to_owned(),
            channel: ChannelMetadata {
                index: 0,
                color_rgba: [1.0, 1.0, 1.0, 1.0],
            },
            source_dtype: IntensityDType::Uint16,
            shape,
            grid_to_world,
            display: default_u16_display(),
            scales: vec![StreamingU16ScaleSpec {
                level: 0,
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world,
                source_scale: None,
                reduction: ScaleReduction::Source,
            }],
        })
        .unwrap();
    layer.write_timepoint(0, 0, &[0, 0, 5, 0]).unwrap();
    writer.finish_streaming_layer(layer).unwrap();
    writer.finish().unwrap();

    let manifest = load_manifest(&package).unwrap();
    let records = &manifest.layers[0].scales[0].bricks.records;
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[0].index,
        BrickIndex {
            t: 0,
            z: 0,
            y: 0,
            x: 0
        }
    );
    assert!(records[0].occupied);
    assert_eq!(records[0].valid_voxel_count, 2);
    assert_eq!(records[0].min, 0.0);
    assert_eq!(records[0].max, 0.0);
    assert_eq!(
        records[1].index,
        BrickIndex {
            t: 0,
            z: 0,
            y: 0,
            x: 1
        }
    );
    assert!(records[1].occupied);
    assert_eq!(records[1].valid_voxel_count, 2);
    assert_eq!(records[1].min, 0.0);
    assert_eq!(records[1].max, 5.0);
}

#[test]
fn dense_layer_writer_marks_zero_only_dense_bricks_valid() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("dense-valid-zero.m4d");
    let shape = Shape4D::new(1, 1, 1, 4).unwrap();
    let grid_to_world = GridToWorld::scale_um(1.0, 1.0, 1.0);

    write_native_u16_multiscale_dataset(
        &package,
        NativeU16MultiscaleDataset {
            id: "dense-valid-zero".to_owned(),
            name: "Dense Valid Zero".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: mirante4d_core::WorldUnit::Micrometer,
            },
            layers: vec![DenseU16MultiscaleLayer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [1.0, 1.0, 1.0, 1.0],
                },
                shape,
                grid_to_world,
                display: default_u16_display(),
                scales: vec![DenseU16Scale {
                    level: 0,
                    shape,
                    brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                    grid_to_world,
                    source_scale: None,
                    reduction: ScaleReduction::Source,
                    values_tzyx: vec![0, 0, 7, 0],
                }],
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let manifest = load_manifest(&package).unwrap();
    let records = &manifest.layers[0].scales[0].bricks.records;
    assert_eq!(records.len(), 2);
    assert!(records[0].occupied);
    assert_eq!(records[0].valid_voxel_count, 2);
    assert_eq!(records[0].min, 0.0);
    assert_eq!(records[0].max, 0.0);
    assert!(records[1].occupied);
    assert_eq!(records[1].valid_voxel_count, 2);
    assert_eq!(records[1].min, 0.0);
    assert_eq!(records[1].max, 7.0);
}

#[test]
fn streaming_layer_writer_rejects_duplicate_timepoint_write() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("streaming.m4d");
    let shape = Shape4D::new(1, 1, 1, 2).unwrap();
    let grid_to_world = GridToWorld::scale_um(1.0, 1.0, 1.0);
    let writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "streaming-test".to_owned(),
        "Streaming Test".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: mirante4d_core::WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    let mut layer = writer
        .begin_streaming_layer(StreamingU16LayerSpec {
            id: "ch0".to_owned(),
            name: "Channel 0".to_owned(),
            channel: ChannelMetadata {
                index: 0,
                color_rgba: [1.0, 1.0, 1.0, 1.0],
            },
            source_dtype: IntensityDType::Uint16,
            shape,
            grid_to_world,
            display: default_u16_display(),
            scales: vec![StreamingU16ScaleSpec {
                level: 0,
                shape,
                brick_shape: shape,
                grid_to_world,
                source_scale: None,
                reduction: ScaleReduction::Source,
            }],
        })
        .unwrap();

    layer.write_timepoint(0, 0, &[1, 2]).unwrap();
    let err = layer.write_timepoint(0, 0, &[3, 4]).unwrap_err();

    assert!(matches!(err, FormatError::DuplicateTimepointWrite { .. }));
}

#[test]
fn streaming_layer_writer_rejects_incomplete_scale_on_finish() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("streaming.m4d");
    let shape = Shape4D::new(2, 1, 1, 2).unwrap();
    let grid_to_world = GridToWorld::scale_um(1.0, 1.0, 1.0);
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "streaming-test".to_owned(),
        "Streaming Test".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: mirante4d_core::WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    let mut layer = writer
        .begin_streaming_layer(StreamingU16LayerSpec {
            id: "ch0".to_owned(),
            name: "Channel 0".to_owned(),
            channel: ChannelMetadata {
                index: 0,
                color_rgba: [1.0, 1.0, 1.0, 1.0],
            },
            source_dtype: IntensityDType::Uint16,
            shape,
            grid_to_world,
            display: default_u16_display(),
            scales: vec![StreamingU16ScaleSpec {
                level: 0,
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world,
                source_scale: None,
                reduction: ScaleReduction::Source,
            }],
        })
        .unwrap();

    layer.write_timepoint(0, 0, &[1, 2]).unwrap();
    let err = writer.finish_streaming_layer(layer).unwrap_err();

    assert!(matches!(err, FormatError::IncompleteScaleWrites { .. }));
}

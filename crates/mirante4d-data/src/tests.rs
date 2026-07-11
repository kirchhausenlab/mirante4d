use std::path::{Path, PathBuf};
use std::time::Duration;

use glam::DVec3;
use mirante4d_domain::{IntensityDType, Shape3D, Shape4D, TimeIndex};
use mirante4d_format::{
    ChannelMetadata, CurrentGridToWorldExt, DenseF32Layer, DenseU16Layer, DenseU16MultiscaleLayer,
    DenseU16Scale, ExistingPackagePolicy, FixtureKind, LayerId, NativeF32Dataset,
    NativeMultiscaleDatasetWriter, NativeU16Dataset, NativeU16MultiscaleDataset, ScaleReduction,
    StreamingU8LayerSpec, StreamingU8ScaleSpec, WorldSpace, WorldUnit, default_f32_display,
    default_u16_display, expected_fixture_value, expected_fixture_value_for_channel, write_fixture,
    write_native_f32_dataset, write_native_u16_dataset, write_native_u16_multiscale_dataset,
};

use super::*;

#[test]
fn runtime_config_defaults_match_documented_safe_policy() {
    let config = DataRuntimeConfig::default();

    assert_eq!(config.volume_cache_budget_bytes, 512 * MIB);
    assert_eq!(config.brick_cache_budget_bytes, 2 * GIB);
    assert_eq!(config.upload_staging_budget_bytes, 512 * MIB);
    assert_eq!(config.max_in_flight_decoded_bytes, 512 * MIB);
}

#[test]
fn runtime_config_derives_staging_and_in_flight_caps_from_brick_budget() {
    let config = DataRuntimeConfig::from_cache_budgets(123, 8 * GIB);

    assert_eq!(config.volume_cache_budget_bytes, 123);
    assert_eq!(config.brick_cache_budget_bytes, 8 * GIB);
    assert_eq!(config.upload_staging_budget_bytes, GIB);
    assert_eq!(config.max_in_flight_decoded_bytes, 2 * GIB);
}

#[test]
fn dataset_diagnostics_report_runtime_config_and_stats() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let diagnostics = dataset.diagnostics().unwrap();

    assert_eq!(diagnostics.config, DataRuntimeConfig::default());
    assert_eq!(diagnostics.stats, DataEngineStats::default());
}

#[test]
fn dataset_diagnostics_preserve_custom_runtime_config() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let config = DataRuntimeConfig::from_cache_budgets(17 * MIB, 9 * GIB);
    let dataset = DatasetHandle::open_with_runtime_config(&root, config).unwrap();
    let diagnostics = dataset.diagnostics().unwrap();

    assert_eq!(dataset.runtime_config(), config);
    assert_eq!(diagnostics.config, config);
    assert_eq!(diagnostics.stats, DataEngineStats::default());
}

#[test]
fn exposes_brick_metadata_without_decoding_payloads() {
    let tempdir = tempfile::tempdir().unwrap();
    let shape = Shape4D::new(1, 4, 4, 4).unwrap();
    let mut values = vec![0u16; shape.element_count().unwrap() as usize];
    values[0] = 7;
    let root = tempdir.path().join("metadata-occupancy.m4d");
    write_native_u16_dataset(
        &root,
        NativeU16Dataset {
            id: "metadata-occupancy".to_owned(),
            name: "Metadata occupancy".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: values,
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();

    let occupied = dataset
        .brick_metadata(
            &layer_id,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let empty = dataset
        .brick_metadata(
            &layer_id,
            TimeIndex::new(0),
            SpatialBrickIndex::new(1, 1, 1),
        )
        .unwrap();
    let stats = dataset.stats().unwrap();

    assert!(occupied.occupied);
    assert_eq!(occupied.max, 7.0);
    assert_eq!(
        occupied.region,
        VolumeRegion::new(0, 0, 0, 2, 2, 2).unwrap()
    );
    assert!(occupied.payload_bytes > 0);
    assert!(empty.occupied);
    assert_eq!(empty.valid_voxel_count, 8);
    assert_eq!(empty.min, 0.0);
    assert_eq!(empty.max, 0.0);
    assert_eq!(empty.region, VolumeRegion::new(2, 2, 2, 2, 2, 2).unwrap());
    assert!(empty.payload_bytes > 0);
    assert_eq!(stats.subset_reads, 0);
    assert_eq!(stats.brick_reads, 0);
}

#[test]
fn brick_read_pool_reports_configured_worker_and_queue_limits() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let pool = BrickReadPool::new(dataset, 3, 7).unwrap();

    assert_eq!(pool.worker_count(), 3);
    assert_eq!(pool.queue_capacity(), 7);
}

#[test]
fn opens_valid_fixture_and_reads_known_values() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();

    assert_eq!(volume.shape, Shape3D::new(16, 16, 16).unwrap());
    assert_eq!(
        volume.voxel(0, 0, 0),
        Some(expected_fixture_value(0, 0, 0, 0))
    );
    assert_eq!(
        volume.voxel(15, 7, 3),
        Some(expected_fixture_value(0, 15, 7, 3))
    );
}

#[test]
fn reads_uint8_stored_layer_as_uint8_volume() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("u8-readable.m4d");
    let shape = Shape4D::new(1, 1, 2, 4).unwrap();
    let grid_to_world = mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0);
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &package,
        "u8-readable".to_owned(),
        "U8 Readable".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: WorldUnit::Micrometer,
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
            display: default_u16_display(),
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
    layer
        .write_timepoint(0, 0, &[0, 1, 2, 3, 250, 251, 252, 253])
        .unwrap();
    writer.finish_streaming_u8_layer(layer).unwrap();
    writer.finish().unwrap();

    let dataset = DatasetHandle::open(&package).unwrap();
    let layer_id = LayerId::new("ch0").unwrap();
    let volume = dataset
        .read_u8_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    assert_eq!(volume.values(), &[0, 1, 2, 3, 250, 251, 252, 253]);

    let brick = dataset
        .read_u8_brick_at_scale(
            &layer_id,
            0,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 1),
        )
        .unwrap();
    assert_eq!(brick.values(), &[2, 3]);

    assert!(matches!(
        dataset.read_u16_volume(&layer_id, TimeIndex::new(0)),
        Err(DataError::UnsupportedDType {
            dtype: IntensityDType::Uint8,
            ..
        })
    ));
}

#[test]
fn reads_float32_stored_layer_as_float32_volume_and_region() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("f32-readable.m4d");
    let shape = Shape4D::new(1, 1, 2, 4).unwrap();
    let values = vec![0.0, 0.25, 1.5, -2.0, 10.0, 11.25, 12.5, 13.75];
    write_native_f32_dataset(
        &package,
        NativeF32Dataset {
            id: "f32-readable".to_owned(),
            name: "F32 Readable".to_owned(),
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
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_f32_display(),
                values_tzyx: values.clone(),
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let dataset = DatasetHandle::open(&package).unwrap();
    let layer_id = LayerId::new("ch0").unwrap();
    let volume = dataset
        .read_f32_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    assert_eq!(volume.shape, Shape3D::new(1, 2, 4).unwrap());
    assert_eq!(volume.values(), values.as_slice());
    assert_eq!(volume.voxel(0, 1, 3), Some(13.75));

    let region = dataset
        .read_f32_region(
            &layer_id,
            TimeIndex::new(0),
            VolumeRegion::new(0, 0, 1, 1, 2, 2).unwrap(),
        )
        .unwrap();
    assert_eq!(region.shape, Shape3D::new(1, 2, 2).unwrap());
    assert_eq!(region.values(), &[0.25, 1.5, 11.25, 12.5]);

    let brick = dataset
        .read_f32_brick(
            &layer_id,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 1, 1),
        )
        .unwrap();
    assert_eq!(brick.scale_level, 0);
    assert_eq!(
        brick.chunk_index,
        BrickIndex {
            t: 0,
            z: 0,
            y: 1,
            x: 1
        }
    );
    assert_eq!(brick.region, VolumeRegion::new(0, 1, 2, 1, 1, 2).unwrap());
    assert_eq!(brick.volume.shape, Shape3D::new(1, 1, 2).unwrap());
    assert_eq!(brick.values(), &[12.5, 13.75]);
    assert_eq!(brick.voxel(0, 0, 1), Some(13.75));

    let stats = dataset.stats().unwrap();
    assert_eq!(stats.subset_reads, 3);
    assert_eq!(stats.decoded_values, 14);
    assert_eq!(stats.decoded_bytes, 14 * std::mem::size_of::<f32>() as u64);
    assert_eq!(stats.brick_reads, 1);
    assert_eq!(stats.decoded_brick_values, 2);
    assert_eq!(
        stats.decoded_brick_bytes,
        2 * std::mem::size_of::<f32>() as u64
    );
    assert!(stats.encoded_payload_bytes_read > 0);
}

#[test]
fn read_u16_volume_rejects_float32_layer_without_lossy_conversion() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("f32-not-u16.m4d");
    let shape = Shape4D::new(1, 1, 1, 2).unwrap();
    write_native_f32_dataset(
        &package,
        NativeF32Dataset {
            id: "f32-not-u16".to_owned(),
            name: "F32 Not U16".to_owned(),
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
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_f32_display(),
                values_tzyx: vec![0.5, 1.5],
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();

    let dataset = DatasetHandle::open(&package).unwrap();
    let err = dataset
        .read_u16_volume(&LayerId::new("ch0").unwrap(), TimeIndex::new(0))
        .unwrap_err();

    assert!(matches!(
        err,
        DataError::UnsupportedDType {
            dtype: IntensityDType::Float32,
            ..
        }
    ));
}

#[test]
fn reads_requested_timepoint() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeU16_8Cube3T, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(2))
        .unwrap();

    assert_eq!(volume.shape, Shape3D::new(8, 8, 8).unwrap());
    assert_eq!(
        volume.voxel(7, 7, 7),
        Some(expected_fixture_value(2, 7, 7, 7))
    );
}

#[test]
fn reads_only_requested_timepoint_subset() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeU16_8Cube3T, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();

    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(2))
        .unwrap();
    let stats = dataset.stats().unwrap();

    assert_eq!(volume.values().len(), 8 * 8 * 8);
    assert_eq!(stats.subset_reads, 1);
    assert_eq!(stats.decoded_values, 8 * 8 * 8);
    assert_eq!(stats.volume_cache_misses, 1);
    assert_eq!(stats.volume_cache_hits, 0);
}

#[test]
fn volume_read_records_encoded_payload_and_decoded_byte_counts() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeU16_8Cube3T, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let region = VolumeRegion::new(0, 0, 0, 8, 8, 8).unwrap();
    let expected_payload_bytes = encoded_payload_bytes_for_timepoint_region(
        &layer_id,
        dataset.scale(&layer_id, 0).unwrap(),
        TimeIndex::new(2),
        &region,
    )
    .unwrap();

    dataset
        .read_u16_volume(&layer_id, TimeIndex::new(2))
        .unwrap();
    let stats = dataset.stats().unwrap();

    assert!(expected_payload_bytes > 0);
    assert_eq!(stats.decoded_values, 8 * 8 * 8);
    assert_eq!(stats.decoded_bytes, 8 * 8 * 8 * 2);
    assert_eq!(stats.encoded_payload_bytes_read, expected_payload_bytes);
}

#[test]
fn neighboring_brick_reads_reuse_cached_shard_index() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(
        tempdir.path(),
        Shape4D::new(1, 4, 4, 4).unwrap(),
        Shape4D::new(1, 2, 2, 2).unwrap(),
    );
    let dataset = DatasetHandle::open_with_cache_budgets(&root, 0, 0).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();

    dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let after_first = dataset.stats().unwrap();
    assert_eq!(after_first.subset_reads, 1);
    assert_eq!(after_first.encoded_shard_payloads_read, 1);
    assert_eq!(after_first.shard_index_cache_misses, 1);
    assert_eq!(after_first.shard_index_cache_hits, 0);
    assert_eq!(after_first.shard_index_cache_entries, 1);

    dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 1),
        )
        .unwrap();
    let after_second = dataset.stats().unwrap();
    assert_eq!(after_second.subset_reads, 2);
    assert_eq!(after_second.encoded_shard_payloads_read, 2);
    assert_eq!(after_second.shard_index_cache_misses, 1);
    assert_eq!(after_second.shard_index_cache_hits, 1);
    assert_eq!(after_second.shard_index_cache_entries, 1);
    assert_eq!(after_second.brick_cache_hits, 0);
}

#[test]
fn brick_cache_reports_resident_bytes_by_stored_dtype() {
    let tempdir = tempfile::tempdir().unwrap();

    let u8_root = write_tiny_u8_dataset(tempdir.path());
    let u8_dataset = DatasetHandle::open_with_cache_budgets(&u8_root, 0, 1024).unwrap();
    let u8_layer = u8_dataset.first_layer_id().unwrap();
    u8_dataset
        .read_u8_brick(
            &u8_layer,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let u8_stats = u8_dataset.stats().unwrap();
    assert_eq!(u8_stats.brick_cache_bytes, 2);
    assert_eq!(u8_stats.brick_cache_u8_bytes, 2);
    assert_eq!(u8_stats.brick_cache_u16_bytes, 0);
    assert_eq!(u8_stats.brick_cache_f32_bytes, 0);

    let u16_root = write_spatially_chunked_dataset(
        tempdir.path(),
        Shape4D::new(1, 4, 4, 4).unwrap(),
        Shape4D::new(1, 2, 2, 2).unwrap(),
    );
    let u16_dataset = DatasetHandle::open_with_cache_budgets(&u16_root, 0, 1024).unwrap();
    let u16_layer = u16_dataset.first_layer_id().unwrap();
    u16_dataset
        .read_u16_brick(
            &u16_layer,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let u16_stats = u16_dataset.stats().unwrap();
    assert_eq!(u16_stats.brick_cache_bytes, 16);
    assert_eq!(u16_stats.brick_cache_u8_bytes, 0);
    assert_eq!(u16_stats.brick_cache_u16_bytes, 16);
    assert_eq!(u16_stats.brick_cache_f32_bytes, 0);

    let f32_root = write_tiny_f32_dataset(tempdir.path());
    let f32_dataset = DatasetHandle::open_with_cache_budgets(&f32_root, 0, 1024).unwrap();
    let f32_layer = f32_dataset.first_layer_id().unwrap();
    f32_dataset
        .read_f32_brick(
            &f32_layer,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let f32_stats = f32_dataset.stats().unwrap();
    assert_eq!(f32_stats.brick_cache_bytes, 8);
    assert_eq!(f32_stats.brick_cache_u8_bytes, 0);
    assert_eq!(f32_stats.brick_cache_u16_bytes, 0);
    assert_eq!(f32_stats.brick_cache_f32_bytes, 8);
}

#[test]
fn reuses_cached_timepoint_volume() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeU16_8Cube3T, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();

    let first = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(1))
        .unwrap();
    let second = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(1))
        .unwrap();
    let stats = dataset.stats().unwrap();

    assert_eq!(second.values(), first.values());
    assert_eq!(stats.subset_reads, 1);
    assert_eq!(stats.decoded_values, 8 * 8 * 8);
    assert_eq!(stats.volume_cache_misses, 1);
    assert_eq!(stats.volume_cache_hits, 1);
}

#[test]
fn evicts_cached_volumes_when_byte_budget_is_exceeded() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeU16_8Cube3T, tempdir.path()).unwrap();
    let one_volume_bytes = 8 * 8 * 8 * std::mem::size_of::<u16>() as u64;
    let dataset = DatasetHandle::open_with_cache_budget(&root, one_volume_bytes).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();

    dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    dataset
        .read_u16_volume(&layer_id, TimeIndex::new(1))
        .unwrap();
    dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let stats = dataset.stats().unwrap();

    assert_eq!(stats.subset_reads, 3);
    assert_eq!(stats.volume_cache_hits, 0);
    assert_eq!(stats.volume_cache_misses, 3);
    assert_eq!(stats.volume_cache_evictions, 2);
    assert_eq!(stats.volume_cache_bytes, one_volume_bytes);
}

#[test]
fn reads_requested_channel_layer() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeMultiChannelU16_8Cube3T2C, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = LayerId::new("ch1").unwrap();
    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(2))
        .unwrap();

    assert_eq!(
        volume.voxel(7, 7, 7),
        Some(expected_fixture_value_for_channel(1, 2, 7, 7, 7))
    );
}

#[test]
fn reads_requested_spatial_region_subset() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let region = VolumeRegion::new(2, 3, 4, 5, 6, 7).unwrap();

    let volume = dataset
        .read_u16_region(&layer_id, TimeIndex::new(0), region)
        .unwrap();
    let stats = dataset.stats().unwrap();

    assert_eq!(volume.shape, Shape3D::new(5, 6, 7).unwrap());
    assert_eq!(
        volume.voxel(0, 0, 0),
        Some(expected_fixture_value(0, 2, 3, 4))
    );
    assert_eq!(
        volume.voxel(4, 5, 6),
        Some(expected_fixture_value(0, 6, 8, 10))
    );
    assert_eq!(stats.subset_reads, 1);
    assert_eq!(stats.decoded_values, 5 * 6 * 7);

    let original_grid_to_world = dataset.layer(&layer_id).unwrap().grid_to_world;
    let expected_world_origin = original_grid_to_world.transform_point_vec(DVec3::new(
        region.x_start as f64,
        region.y_start as f64,
        region.z_start as f64,
    ));
    let actual_world_origin = volume.grid_to_world.transform_point_vec(DVec3::ZERO);
    assert!(
        (actual_world_origin - expected_world_origin).length() < 1.0e-12,
        "region local origin must map to original grid offset"
    );
}

#[test]
fn rejects_out_of_bounds_spatial_region() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let region = VolumeRegion::new(15, 0, 0, 2, 1, 1).unwrap();

    let err = dataset
        .read_u16_region(&layer_id, TimeIndex::new(0), region)
        .unwrap_err();

    assert!(matches!(err, DataError::RegionOutOfBounds { .. }));
}

#[test]
fn rejects_zero_sized_spatial_region() {
    let err = VolumeRegion::new(0, 0, 0, 0, 1, 1).unwrap_err();

    assert!(matches!(err, DataError::InvalidRegionSize { .. }));
}

#[test]
fn reports_spatial_brick_grid_shape() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(
        tempdir.path(),
        Shape4D::new(1, 5, 6, 7).unwrap(),
        Shape4D::new(1, 2, 3, 4).unwrap(),
    );
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();

    let grid = dataset.brick_grid_shape(&layer_id).unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();

    assert_eq!(grid, Shape3D::new(3, 2, 2).unwrap());
    assert_eq!(brick_shape, Shape3D::new(2, 3, 4).unwrap());
}

#[test]
fn reads_scale_addressed_volume_and_brick() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_dataset(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();

    assert_eq!(dataset.scale_count(&layer_id).unwrap(), 2);
    assert_eq!(
        dataset.scale_shape(&layer_id, 1).unwrap(),
        Shape3D::new(2, 2, 2).unwrap()
    );
    assert_eq!(
        dataset.brick_shape_at_scale(&layer_id, 1).unwrap(),
        Shape3D::new(2, 2, 2).unwrap()
    );

    let volume = dataset
        .read_u16_volume_at_scale(&layer_id, 1, TimeIndex::new(0))
        .unwrap();
    let brick = dataset
        .read_u16_brick_at_scale(
            &layer_id,
            1,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();

    assert_eq!(volume.scale_level, 1);
    assert_eq!(volume.shape, Shape3D::new(2, 2, 2).unwrap());
    assert_eq!(volume.voxel(1, 1, 1), Some(multiscale_s1_value(0, 1, 1, 1)));
    assert_eq!(brick.scale_level, 1);
    assert_eq!(brick.volume.scale_level, 1);
    assert_eq!(brick.volume.shape, Shape3D::new(2, 2, 2).unwrap());
    assert_eq!(brick.voxel(1, 1, 1), Some(multiscale_s1_value(0, 1, 1, 1)));

    let s1_origin = dataset
        .scale_grid_to_world(&layer_id, 1)
        .unwrap()
        .transform_point_vec(DVec3::ZERO);
    let brick_origin = brick.volume.grid_to_world.transform_point_vec(DVec3::ZERO);
    assert!((brick_origin - s1_origin).length() < 1.0e-12);
}

#[test]
fn scale_addressed_brick_cache_does_not_alias_between_scales() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_dataset(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();

    let s0 = dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let s1_first = dataset
        .read_u16_brick_at_scale(
            &layer_id,
            1,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let s1_second = dataset
        .read_u16_brick_at_scale(
            &layer_id,
            1,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let stats = dataset.stats().unwrap();

    assert_eq!(s0.scale_level, 0);
    assert_eq!(s1_first.scale_level, 1);
    assert_eq!(s1_second.values(), s1_first.values());
    assert_ne!(s1_first.values(), s0.values());
    assert_eq!(stats.brick_reads, 2);
    assert_eq!(stats.brick_cache_hits, 1);
    assert_eq!(stats.brick_cache_misses, 2);
}

#[test]
fn reads_edge_spatial_brick_subset_and_metadata() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(
        tempdir.path(),
        Shape4D::new(1, 5, 6, 7).unwrap(),
        Shape4D::new(1, 2, 3, 4).unwrap(),
    );
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let brick_index = SpatialBrickIndex::new(2, 1, 1);

    let brick = dataset
        .read_u16_brick(&layer_id, TimeIndex::new(0), brick_index)
        .unwrap();
    let stats = dataset.stats().unwrap();

    assert_eq!(brick.brick_index, brick_index);
    assert_eq!(
        brick.chunk_index,
        BrickIndex {
            t: 0,
            z: 2,
            y: 1,
            x: 1,
        }
    );
    assert_eq!(brick.region, VolumeRegion::new(4, 3, 4, 1, 3, 3).unwrap());
    assert!(brick.occupied);
    assert_eq!(brick.volume.shape, Shape3D::new(1, 3, 3).unwrap());
    assert_eq!(
        brick.voxel(0, 0, 0),
        Some(expected_fixture_value(0, 4, 3, 4))
    );
    assert_eq!(
        brick.voxel(0, 2, 2),
        Some(expected_fixture_value(0, 4, 5, 6))
    );
    assert_eq!(stats.subset_reads, 1);
    assert_eq!(stats.decoded_values, 9);
    assert_eq!(stats.brick_reads, 1);
    assert_eq!(stats.decoded_brick_values, 9);
    assert_eq!(stats.decoded_bytes, 18);
    assert_eq!(stats.decoded_brick_bytes, 18);
    let metadata = dataset
        .brick_metadata_at_scale(&layer_id, 0, TimeIndex::new(0), brick.brick_index)
        .unwrap();
    assert_eq!(stats.encoded_payload_bytes_read, metadata.payload_bytes);
    assert_eq!(stats.brick_cache_misses, 1);
    assert_eq!(stats.brick_cache_hits, 0);
    assert_eq!(stats.brick_cache_bytes, 18);

    let original_grid_to_world = dataset.layer(&layer_id).unwrap().grid_to_world;
    let expected_world_origin =
        original_grid_to_world.transform_point_vec(DVec3::new(4.0, 3.0, 4.0));
    let actual_world_origin = brick.volume.grid_to_world.transform_point_vec(DVec3::ZERO);
    assert!(
        (actual_world_origin - expected_world_origin).length() < 1.0e-12,
        "brick local origin must map to original grid offset"
    );
}

#[test]
fn reads_partial_brick_region_without_full_brick_cache_aliasing() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(
        tempdir.path(),
        Shape4D::new(1, 4, 4, 4).unwrap(),
        Shape4D::new(1, 2, 2, 2).unwrap(),
    );
    let dataset = DatasetHandle::open_with_cache_budgets(&root, 0, 1024).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let brick_index = SpatialBrickIndex::new(1, 0, 0);
    let region = VolumeRegion::new(3, 0, 0, 1, 2, 2).unwrap();

    let brick = dataset
        .read_u16_brick_region_at_scale_cancellable(
            &layer_id,
            0,
            TimeIndex::new(0),
            brick_index,
            region,
            || false,
        )
        .unwrap()
        .unwrap();
    let stats = dataset.stats().unwrap();

    assert_eq!(brick.brick_index, brick_index);
    assert_eq!(brick.region, region);
    assert_eq!(brick.volume.shape, Shape3D::new(1, 2, 2).unwrap());
    assert_eq!(
        brick.values(),
        &[
            expected_fixture_value(0, 3, 0, 0),
            expected_fixture_value(0, 3, 0, 1),
            expected_fixture_value(0, 3, 1, 0),
            expected_fixture_value(0, 3, 1, 1),
        ]
    );
    assert_eq!(stats.subset_reads, 1);
    assert_eq!(stats.decoded_values, 4);
    assert_eq!(stats.brick_reads, 1);
    assert_eq!(stats.decoded_brick_values, 4);
    assert_eq!(stats.brick_cache_hits, 0);
    assert_eq!(stats.brick_cache_misses, 0);
    assert_eq!(stats.brick_cache_bytes, 0);
}

#[test]
fn reuses_cached_spatial_brick() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(
        tempdir.path(),
        Shape4D::new(1, 4, 4, 4).unwrap(),
        Shape4D::new(1, 2, 2, 2).unwrap(),
    );
    let one_brick_bytes = 2 * 2 * 2 * std::mem::size_of::<u16>() as u64;
    let dataset = DatasetHandle::open_with_cache_budgets(&root, 0, one_brick_bytes).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let brick_index = SpatialBrickIndex::new(0, 0, 0);

    let first = dataset
        .read_u16_brick(&layer_id, TimeIndex::new(0), brick_index)
        .unwrap();
    let second = dataset
        .read_u16_brick(&layer_id, TimeIndex::new(0), brick_index)
        .unwrap();
    let stats = dataset.stats().unwrap();

    assert_eq!(second.values(), first.values());
    assert_eq!(stats.subset_reads, 1);
    assert_eq!(stats.brick_reads, 1);
    assert_eq!(stats.brick_cache_misses, 1);
    assert_eq!(stats.brick_cache_hits, 1);
    assert_eq!(stats.brick_cache_evictions, 0);
    assert_eq!(stats.brick_cache_bytes, one_brick_bytes);
}

#[test]
fn evicts_cached_spatial_bricks_when_byte_budget_is_exceeded() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(
        tempdir.path(),
        Shape4D::new(1, 4, 4, 4).unwrap(),
        Shape4D::new(1, 2, 2, 2).unwrap(),
    );
    let one_brick_bytes = 2 * 2 * 2 * std::mem::size_of::<u16>() as u64;
    let dataset = DatasetHandle::open_with_cache_budgets(&root, 0, one_brick_bytes).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();

    dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 1),
        )
        .unwrap();
    dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let stats = dataset.stats().unwrap();

    assert_eq!(stats.subset_reads, 3);
    assert_eq!(stats.brick_reads, 3);
    assert_eq!(stats.brick_cache_hits, 0);
    assert_eq!(stats.brick_cache_misses, 3);
    assert_eq!(stats.brick_cache_evictions, 2);
    assert_eq!(stats.brick_cache_bytes, one_brick_bytes);
}

#[test]
fn rejects_out_of_bounds_spatial_brick() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(
        tempdir.path(),
        Shape4D::new(1, 4, 4, 4).unwrap(),
        Shape4D::new(1, 2, 2, 2).unwrap(),
    );
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();

    let err = dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex::new(0),
            SpatialBrickIndex::new(2, 0, 0),
        )
        .unwrap_err();

    assert!(matches!(err, DataError::BrickIndexOutOfBounds { .. }));
}

#[test]
fn reads_timepoint_local_brick_from_temporal_chunk_grid() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::TimeU16_8Cube3T, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();

    let brick = dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex::new(2),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();

    assert_eq!(brick.chunk_index.t, 2);
    assert_eq!(brick.volume.shape, Shape3D::new(8, 8, 8).unwrap());
    assert_eq!(
        brick.voxel(7, 7, 7),
        Some(expected_fixture_value(2, 7, 7, 7))
    );
}

#[test]
fn async_worker_reads_spatial_brick() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(
        tempdir.path(),
        Shape4D::new(1, 4, 4, 4).unwrap(),
        Shape4D::new(1, 2, 2, 2).unwrap(),
    );
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let pool = BrickReadPool::new(dataset.clone(), 1, 4).unwrap();

    let ticket = pool
        .submit_brick(
            layer_id,
            TimeIndex::new(0),
            SpatialBrickIndex::new(1, 1, 1),
            BrickRequestPriority::CurrentFrame,
        )
        .unwrap();
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    let stats = dataset.stats().unwrap();

    assert_eq!(outcome.request_id, ticket.request_id);
    assert_eq!(outcome.generation_id, ticket.generation_id);
    match outcome.status {
        BrickReadStatus::Completed(BrickReadPayload::U16(brick)) => {
            assert_eq!(brick.region, VolumeRegion::new(2, 2, 2, 2, 2, 2).unwrap());
            assert_eq!(
                brick.voxel(0, 0, 0),
                Some(expected_fixture_value(0, 2, 2, 2))
            );
        }
        other => panic!("expected completed brick, got {other:?}"),
    }
    assert_eq!(stats.brick_requests_queued, 1);
    assert_eq!(stats.brick_requests_completed, 1);
    assert_eq!(stats.brick_requests_cancelled, 0);
    assert_eq!(stats.brick_reads, 1);
}

#[test]
fn cross_section_chunk_read_pool_reads_full_chunk_without_histogram_sampling() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(
        tempdir.path(),
        Shape4D::new(1, 4, 4, 4).unwrap(),
        Shape4D::new(1, 2, 2, 2).unwrap(),
    );
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let pool = CrossSectionChunkReadPool::new(dataset.clone(), 1, 4).unwrap();

    let ticket = pool
        .submit_chunk_for_generation(
            pool.active_generation(),
            CrossSectionChunkReadSpec {
                layer_id,
                scale_level: 0,
                timepoint: TimeIndex::new(0),
                brick_index: SpatialBrickIndex::new(1, 1, 1),
                priority: BrickRequestPriority::CurrentFrame,
                queue_priority: 5,
                cancellation: CancellationToken::new(),
            },
        )
        .unwrap();
    let outcome = pool
        .try_recv()
        .or_else(|| {
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                if let Some(outcome) = pool.try_recv() {
                    break Some(outcome);
                }
                if std::time::Instant::now() >= deadline {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        })
        .unwrap();
    let stats = dataset.stats().unwrap();

    assert_eq!(outcome.request_id, ticket.request_id);
    assert_eq!(outcome.generation_id, ticket.generation_id);
    assert_eq!(outcome.sample_region, None);
    assert_eq!(outcome.histogram_sample, None);
    match outcome.status {
        BrickReadStatus::Completed(BrickReadPayload::U16(brick)) => {
            assert_eq!(brick.region, VolumeRegion::new(2, 2, 2, 2, 2, 2).unwrap());
            assert_eq!(
                brick.voxel(0, 0, 0),
                Some(expected_fixture_value(0, 2, 2, 2))
            );
        }
        other => panic!("expected completed cross-section chunk, got {other:?}"),
    }
    assert_eq!(stats.brick_requests_queued, 1);
    assert_eq!(stats.brick_requests_completed, 1);
    assert_eq!(stats.brick_reads, 1);
}

#[test]
fn async_worker_reads_scale_addressed_brick() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_multiscale_dataset(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let pool = BrickReadPool::new(dataset.clone(), 1, 4).unwrap();

    let ticket = pool
        .submit_brick_at_scale(
            layer_id,
            1,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 0),
            BrickRequestPriority::CurrentFrame,
        )
        .unwrap();
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    let stats = dataset.stats().unwrap();

    assert_eq!(ticket.scale_level, 1);
    assert_eq!(outcome.request_id, ticket.request_id);
    assert_eq!(outcome.scale_level, 1);
    match outcome.status {
        BrickReadStatus::Completed(BrickReadPayload::U16(brick)) => {
            assert_eq!(brick.scale_level, 1);
            assert_eq!(brick.voxel(1, 1, 1), Some(multiscale_s1_value(0, 1, 1, 1)));
        }
        other => panic!("expected completed scale 1 brick, got {other:?}"),
    }
    assert_eq!(stats.brick_requests_queued, 1);
    assert_eq!(stats.brick_requests_completed, 1);
    assert_eq!(stats.brick_reads, 1);
}

#[test]
fn async_worker_reads_float32_brick_without_u16_conversion_and_reuses_cache() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = tempdir.path().join("f32-worker.m4d");
    let shape = Shape4D::new(1, 1, 2, 4).unwrap();
    write_native_f32_dataset(
        &package,
        NativeF32Dataset {
            id: "f32-worker".to_owned(),
            name: "F32 Worker".to_owned(),
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
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_f32_display(),
                values_tzyx: vec![0.0, 0.25, 1.5, -2.0, 10.0, 11.25, 12.5, 13.75],
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    let dataset = DatasetHandle::open(&package).unwrap();
    let layer_id = LayerId::new("ch0").unwrap();
    let pool = BrickReadPool::new(dataset.clone(), 1, 4).unwrap();

    for _ in 0..2 {
        pool.submit_brick(
            layer_id.clone(),
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 1, 1),
            BrickRequestPriority::CurrentFrame,
        )
        .unwrap();
        let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
        match outcome.status {
            BrickReadStatus::Completed(BrickReadPayload::F32(brick)) => {
                assert_eq!(brick.values(), &[12.5, 13.75]);
            }
            other => panic!("expected completed float32 brick, got {other:?}"),
        }
    }
    let stats = dataset.stats().unwrap();

    assert_eq!(stats.brick_requests_completed, 2);
    assert_eq!(stats.brick_reads, 1);
    assert_eq!(stats.brick_cache_misses, 1);
    assert_eq!(stats.brick_cache_hits, 1);
    assert_eq!(
        stats.decoded_brick_bytes,
        2 * std::mem::size_of::<f32>() as u64
    );
}

#[test]
fn async_worker_reuses_brick_cache() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(
        tempdir.path(),
        Shape4D::new(1, 4, 4, 4).unwrap(),
        Shape4D::new(1, 2, 2, 2).unwrap(),
    );
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let pool = BrickReadPool::new(dataset.clone(), 1, 4).unwrap();

    pool.submit_brick(
        layer_id.clone(),
        TimeIndex::new(0),
        SpatialBrickIndex::new(0, 0, 0),
        BrickRequestPriority::CurrentFrame,
    )
    .unwrap();
    assert!(matches!(
        pool.recv_timeout(Duration::from_secs(2)).unwrap().status,
        BrickReadStatus::Completed(_)
    ));
    pool.submit_brick(
        layer_id,
        TimeIndex::new(0),
        SpatialBrickIndex::new(0, 0, 0),
        BrickRequestPriority::CurrentFrame,
    )
    .unwrap();
    assert!(matches!(
        pool.recv_timeout(Duration::from_secs(2)).unwrap().status,
        BrickReadStatus::Completed(_)
    ));
    let stats = dataset.stats().unwrap();

    assert_eq!(stats.brick_requests_completed, 2);
    assert_eq!(stats.brick_reads, 1);
    assert_eq!(stats.brick_cache_misses, 1);
    assert_eq!(stats.brick_cache_hits, 1);
}

#[test]
fn async_worker_cancellation_does_not_populate_cache() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(
        tempdir.path(),
        Shape4D::new(1, 4, 4, 4).unwrap(),
        Shape4D::new(1, 2, 2, 2).unwrap(),
    );
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let pool = BrickReadPool::new(dataset.clone(), 1, 4).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    pool.submit_brick_with_token(
        layer_id,
        TimeIndex::new(0),
        SpatialBrickIndex::new(0, 0, 0),
        BrickRequestPriority::Prefetch,
        cancellation,
    )
    .unwrap();
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    let stats = dataset.stats().unwrap();

    assert!(matches!(outcome.status, BrickReadStatus::Cancelled));
    assert_eq!(stats.brick_requests_queued, 1);
    assert_eq!(stats.brick_requests_cancelled, 1);
    assert_eq!(stats.brick_reads, 0);
    assert_eq!(stats.subset_reads, 0);
    assert_eq!(stats.brick_cache_bytes, 0);
}

#[test]
fn async_worker_rejects_stale_generation_without_reading() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_spatially_chunked_dataset(
        tempdir.path(),
        Shape4D::new(1, 4, 4, 4).unwrap(),
        Shape4D::new(1, 2, 2, 2).unwrap(),
    );
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let pool = BrickReadPool::new(dataset.clone(), 1, 4).unwrap();

    pool.submit_brick_for_generation(
        DataGenerationId(0),
        layer_id,
        TimeIndex::new(0),
        SpatialBrickIndex::new(0, 0, 0),
        BrickRequestPriority::Prefetch,
        CancellationToken::new(),
    )
    .unwrap();
    let outcome = pool.recv_timeout(Duration::from_secs(2)).unwrap();
    let stats = dataset.stats().unwrap();

    assert!(matches!(outcome.status, BrickReadStatus::Stale));
    assert_eq!(stats.brick_requests_queued, 1);
    assert_eq!(stats.brick_requests_stale, 1);
    assert_eq!(stats.brick_reads, 0);
    assert_eq!(stats.subset_reads, 0);
}

#[test]
fn rejects_out_of_range_timepoint() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let err = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(1))
        .unwrap_err();

    assert!(matches!(err, DataError::TimepointOutOfRange { .. }));
}

fn write_spatially_chunked_dataset(
    output_root: &Path,
    shape: Shape4D,
    brick_shape: Shape4D,
) -> PathBuf {
    let package_root = output_root.join("spatially-chunked.m4d");
    write_native_u16_dataset(
        &package_root,
        NativeU16Dataset {
            id: "spatially-chunked-fixture".to_owned(),
            name: "Spatially chunked fixture".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            layers: vec![DenseU16Layer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
                },
                shape,
                brick_shape,
                grid_to_world: mirante4d_format::grid_to_world_scale_um(0.2, 0.3, 0.5),
                display: default_u16_display(),
                values_tzyx: fixture_values(shape),
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_tiny_u8_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("tiny-u8.m4d");
    let shape = Shape4D::new(1, 1, 2, 4).unwrap();
    let grid_to_world = mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0);
    let mut writer = NativeMultiscaleDatasetWriter::create(
        &package_root,
        "tiny-u8".to_owned(),
        "Tiny U8".to_owned(),
        WorldSpace {
            name: "sample".to_owned(),
            unit: WorldUnit::Micrometer,
        },
        ExistingPackagePolicy::Replace,
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
            display: default_u16_display(),
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
    layer
        .write_timepoint(0, 0, &[0, 1, 2, 3, 250, 251, 252, 253])
        .unwrap();
    writer.finish_streaming_u8_layer(layer).unwrap();
    writer.finish().unwrap();
    package_root
}

fn write_tiny_f32_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("tiny-f32.m4d");
    let shape = Shape4D::new(1, 1, 2, 4).unwrap();
    write_native_f32_dataset(
        &package_root,
        NativeF32Dataset {
            id: "tiny-f32".to_owned(),
            name: "Tiny F32".to_owned(),
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
                shape,
                brick_shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                grid_to_world: mirante4d_format::grid_to_world_scale_um(1.0, 1.0, 1.0),
                display: default_f32_display(),
                values_tzyx: vec![0.0, 0.25, 1.5, -2.0, 10.0, 11.25, 12.5, 13.75],
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn write_multiscale_dataset(output_root: &Path) -> PathBuf {
    let package_root = output_root.join("multiscale.m4d");
    let s0_shape = Shape4D::new(1, 4, 4, 4).unwrap();
    let s1_shape = Shape4D::new(1, 2, 2, 2).unwrap();
    let s0_grid_to_world = mirante4d_format::grid_to_world_scale_um(0.2, 0.3, 0.5);
    let s1_grid_to_world = s0_grid_to_world
        .downsampled_integer_centered(2, 2, 2)
        .unwrap();
    write_native_u16_multiscale_dataset(
        &package_root,
        NativeU16MultiscaleDataset {
            id: "multiscale-fixture".to_owned(),
            name: "Multiscale fixture".to_owned(),
            world_space: WorldSpace {
                name: "sample".to_owned(),
                unit: WorldUnit::Micrometer,
            },
            layers: vec![DenseU16MultiscaleLayer {
                id: "ch0".to_owned(),
                name: "Channel 0".to_owned(),
                channel: ChannelMetadata {
                    index: 0,
                    color_rgba: [0.0, 1.0, 0.0, 1.0],
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
                        values_tzyx: fixture_values(s0_shape),
                    },
                    DenseU16Scale {
                        level: 1,
                        shape: s1_shape,
                        brick_shape: Shape4D::new(1, 2, 2, 2).unwrap(),
                        grid_to_world: s1_grid_to_world,
                        source_scale: Some(0),
                        reduction: ScaleReduction::Mean,
                        values_tzyx: multiscale_s1_values(s1_shape),
                    },
                ],
            }],
        },
        ExistingPackagePolicy::Replace,
    )
    .unwrap();
    package_root
}

fn fixture_values(shape: Shape4D) -> Vec<u16> {
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    for t in 0..shape.t() {
        for z in 0..shape.z() {
            for y in 0..shape.y() {
                for x in 0..shape.x() {
                    values.push(expected_fixture_value(t, z, y, x));
                }
            }
        }
    }
    values
}

fn multiscale_s1_values(shape: Shape4D) -> Vec<u16> {
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    for t in 0..shape.t() {
        for z in 0..shape.z() {
            for y in 0..shape.y() {
                for x in 0..shape.x() {
                    values.push(multiscale_s1_value(t, z, y, x));
                }
            }
        }
    }
    values
}

fn multiscale_s1_value(t: u64, z: u64, y: u64, x: u64) -> u16 {
    (40_000 + t * 1_000 + z * 100 + y * 10 + x) as u16
}

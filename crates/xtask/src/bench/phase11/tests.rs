use super::*;

#[test]
fn phase11_interaction_camera_sequence_covers_required_scenarios() {
    let base =
        benchmark_camera_for_shape(Shape3D::new(30, 20, 10).unwrap(), GridToWorld::identity());
    let sequence = phase11_interaction_camera_sequence(base, 2);

    assert_eq!(sequence.len(), 7);
    assert_eq!(sequence[0].scenario, "first_visible");
    assert_eq!(sequence[1].scenario, "orbit");
    assert_eq!(sequence[3].scenario, "pan");
    assert_eq!(sequence[5].scenario, "zoom");
    assert_ne!(sequence[1].camera.orientation(), base.orientation());
    assert_ne!(sequence[3].camera.target(), base.target());
    assert!(
        benchmark_camera_world_per_screen_point(sequence[5].camera)
            < benchmark_camera_world_per_screen_point(base)
    );
}

#[test]
fn phase11_viewport_matrix_covers_required_default_scenarios() {
    let scenarios =
        phase11_viewport_matrix_for_shape(Shape3D::new(600, 1148, 998).unwrap()).unwrap();
    let labels = scenarios
        .iter()
        .map(|scenario| scenario.label.as_str())
        .collect::<Vec<_>>();
    let viewports = scenarios
        .iter()
        .map(|scenario| (scenario.viewport.width, scenario.viewport.height))
        .collect::<Vec<_>>();

    assert!(labels.contains(&"square_512"));
    assert!(labels.contains(&"hd_720p"));
    assert!(labels.contains(&"full_hd_1080p"));
    assert!(labels.contains(&"default_package_capped"));
    assert!(viewports.contains(&(512, 512)));
    assert!(viewports.contains(&(1280, 720)));
    assert!(viewports.contains(&(1920, 1080)));
    assert!(viewports.contains(&(998, 1024)));
}

#[test]
fn phase11_decoded_byte_estimator_uses_stored_dtype_size() {
    assert_eq!(
        phase11_decoded_bytes_per_voxel(IntensityDType::Uint8).unwrap(),
        1
    );
    assert_eq!(
        phase11_decoded_bytes_per_voxel(IntensityDType::Uint16).unwrap(),
        2
    );
    assert_eq!(
        phase11_decoded_bytes_per_voxel(IntensityDType::Float32).unwrap(),
        4
    );
}

#[test]
fn phase11_u8_resident_wrapper_renders_without_u16_read_path() {
    let dataset_id = mirante4d_format::DatasetId::new("bench-u8").unwrap();
    let layer_id = LayerId::new("ch0").unwrap();
    let shape = Shape3D::new(1, 2, 2).unwrap();
    let grid_to_world = GridToWorld::identity();
    let volume = mirante4d_data::DenseVolumeU8::new(
        dataset_id,
        layer_id.clone(),
        0,
        TimeIndex::new(0),
        shape,
        grid_to_world,
        vec![0, 64, 128, 255],
    )
    .unwrap();
    let brick = mirante4d_data::VolumeBrickU8 {
        scale_level: 0,
        brick_index: SpatialBrickIndex::new(0, 0, 0),
        chunk_index: mirante4d_format::BrickIndex {
            t: 0,
            z: 0,
            y: 0,
            x: 0,
        },
        region: mirante4d_data::VolumeRegion::new(0, 0, 0, 1, 2, 2).unwrap(),
        occupied: true,
        valid_voxel_count: 4,
        min: 0.0,
        max: 255.0,
        volume,
    };
    let resident =
        Phase11ResidentBrickSet::cpu_only(Phase11CpuResidentBrickSet::U8(ResidentBrickSetU8::new(
            layer_id,
            TimeIndex::new(0),
            shape,
            grid_to_world,
            vec![brick],
        )));
    let camera = benchmark_camera_frame(benchmark_camera_for_shape(shape, grid_to_world));
    let viewport = RenderViewport::new(8, 8).unwrap();

    let (_image, diagnostics) = resident
        .render_cpu(camera, viewport, CameraRenderMode::Mip)
        .unwrap();

    assert_eq!(resident.stored_dtype_label(), "Uint8");
    assert!(diagnostics.frame.nonzero_pixels > 0);
}

#[test]
fn phase11_f32_resident_wrapper_renders_without_integer_conversion() {
    let dataset_id = mirante4d_format::DatasetId::new("bench-f32").unwrap();
    let layer_id = LayerId::new("ch0").unwrap();
    let shape = Shape3D::new(1, 2, 2).unwrap();
    let grid_to_world = GridToWorld::identity();
    let volume = mirante4d_data::DenseVolumeF32::new(
        dataset_id,
        layer_id.clone(),
        0,
        TimeIndex::new(0),
        shape,
        grid_to_world,
        vec![0.0, 0.25, 0.5, 1.0],
    )
    .unwrap();
    let brick = mirante4d_data::VolumeBrickF32 {
        scale_level: 0,
        brick_index: SpatialBrickIndex::new(0, 0, 0),
        chunk_index: mirante4d_format::BrickIndex {
            t: 0,
            z: 0,
            y: 0,
            x: 0,
        },
        region: mirante4d_data::VolumeRegion::new(0, 0, 0, 1, 2, 2).unwrap(),
        occupied: true,
        valid_voxel_count: 4,
        min: 0.0,
        max: 1.0,
        volume,
    };
    let resident = Phase11ResidentBrickSet::cpu_only(Phase11CpuResidentBrickSet::F32(
        ResidentBrickSetF32::new(
            layer_id,
            TimeIndex::new(0),
            shape,
            grid_to_world,
            vec![brick],
        ),
    ));
    let camera = benchmark_camera_frame(benchmark_camera_for_shape(shape, grid_to_world));
    let viewport = RenderViewport::new(8, 8).unwrap();

    let summary = resident.render_cpu_mip_summary(camera, viewport).unwrap();

    assert_eq!(resident.stored_dtype_label(), "Float32");
    assert!(summary.nonzero_pixels > 0);
    assert_eq!(summary.max_value, json!(1.0));
}

#[test]
fn phase11_f32_fixture_read_dispatch_uses_f32_bricks() {
    let tempdir = tempfile::tempdir().unwrap();
    let package = mirante4d_format::write_fixture(
        mirante4d_format::FixtureKind::BasicF32_8Cube,
        tempdir.path(),
    )
    .unwrap();
    let dataset = DatasetHandle::open(&package).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let stored_dtype = phase11_stored_dtype_for_layer(&dataset, &layer_id).unwrap();
    let volume_shape = dataset.scale_shape(&layer_id, 0).unwrap();
    let grid_to_world = dataset.scale_grid_to_world(&layer_id, 0).unwrap();

    let resident = phase11_read_resident_for_layer(
        &dataset,
        &layer_id,
        Phase11ResidentReadInput {
            stored_dtype,
            scale_level: 0,
            timepoint: TimeIndex::new(0),
            volume_shape,
            grid_to_world,
        },
        &[SpatialBrickIndex::new(0, 0, 0)],
    )
    .unwrap();
    let camera = benchmark_camera_frame(benchmark_camera_for_shape(volume_shape, grid_to_world));
    let viewport = RenderViewport::new(8, 8).unwrap();
    let summary = resident.render_cpu_mip_summary(camera, viewport).unwrap();
    let stats = dataset.stats().unwrap();
    let leases = resident
        .leases
        .as_deref()
        .expect("package benchmark reads must retain runtime-issued semantic leases");

    assert!(matches!(&resident.cpu, Phase11CpuResidentBrickSet::F32(_)));
    assert!(leases.bridge.is_complete());
    assert_eq!(leases.bridge.retained_len(), 1);
    assert_eq!(resident.stored_dtype_label(), "Float32");
    assert!(summary.nonzero_pixels > 0);
    assert_eq!(summary.max_value, json!(1.0));
    assert_eq!(stats.brick_cache_u16_bytes, 0);
    assert!(stats.brick_cache_f32_bytes > 0);
}

#[test]
fn phase11_sparse_empty_fixture_leaves_most_source_bricks_empty() {
    let shape = Shape4D::new(1, 128, 128, 128).unwrap();
    let chunk_shape = Shape4D::new(1, 16, 16, 16).unwrap();
    let values = phase11_sparse_empty_values(shape).unwrap();
    let (occupied, total) =
        phase11_sparse_occupied_brick_count(&values, shape, chunk_shape).unwrap();

    assert!(occupied > 0);
    assert_eq!(total, 512);
    assert!(
        occupied * 20 < total,
        "sparse fixture should keep most source bricks empty, got {occupied}/{total}"
    );
}

#[test]
fn phase11_sparse_empty_downsample_preserves_multiscale_sparsity() {
    let s0_shape = Shape4D::new(1, 128, 128, 128).unwrap();
    let chunk_shape = Shape4D::new(1, 16, 16, 16).unwrap();
    let s0_values = phase11_sparse_empty_values(s0_shape).unwrap();
    let (s1_shape, s1_values) = phase11_downsample_u16_mean2(s0_shape, &s0_values).unwrap();
    let (s2_shape, s2_values) = phase11_downsample_u16_mean2(s1_shape, &s1_values).unwrap();
    let (s1_occupied, s1_total) =
        phase11_sparse_occupied_brick_count(&s1_values, s1_shape, chunk_shape).unwrap();
    let (s2_occupied, s2_total) =
        phase11_sparse_occupied_brick_count(&s2_values, s2_shape, chunk_shape).unwrap();

    assert_eq!(s1_shape, Shape4D::new(1, 64, 64, 64).unwrap());
    assert_eq!(s2_shape, Shape4D::new(1, 32, 32, 32).unwrap());
    assert!(s1_occupied > 0);
    assert!(s2_occupied > 0);
    assert!(s1_occupied * 4 < s1_total);
    assert!(s2_occupied < s2_total);
}

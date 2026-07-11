use mirante4d_core::{DatasetId, GridToWorld, LayerId, Shape4D, TimeIndex, WorldSpace, WorldUnit};
use mirante4d_data::{
    DatasetHandle, DenseVolumeF32, DenseVolumeU8, DenseVolumeU16, SpatialBrickIndex,
    VolumeBrickU16, VolumeRegion,
};
use mirante4d_format::{
    ChannelMetadata, DenseU16Layer, ExistingPackagePolicy, FixtureKind, NativeU16Dataset,
    default_u16_display, write_fixture, write_native_u16_dataset,
};

use crate::camera_mip::{
    CameraRenderModeF32, render_camera, render_camera_mip, render_camera_with_quality,
};
use crate::cpu::render_mip_z;
use crate::{
    CameraRenderMode, CameraRenderQuality, RenderViewport, ResidentBrickSetF32, ResidentBrickSetU8,
    ResidentBrickSetU16, render_camera_f32_from_bricks,
};

use super::test_support::*;
use super::volume_cache::{
    GPU_SAMPLE_INVALID_FLAG, render_upload_samples_u16, render_upload_values_f32,
};
use super::*;

#[path = "tests_atlas_cache_key.rs"]
mod tests_atlas_cache_key;

fn resident_u16_constant_brick(
    layer_id: &str,
    value: u16,
    occupied: bool,
) -> (DenseVolumeU16, ResidentBrickSetU16, Shape3D, Shape3D) {
    let layer_id = LayerId::new(layer_id).unwrap();
    let shape = Shape3D::new(4, 4, 4).unwrap();
    let values = vec![value; shape.element_count().unwrap() as usize];
    let volume = DenseVolumeU16::new(
        DatasetId::new("gpu-brick-skip-diagnostics").unwrap(),
        layer_id.clone(),
        0,
        TimeIndex(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap();
    let valid_voxel_count = if occupied {
        shape.element_count().unwrap()
    } else {
        0
    };
    let brick = VolumeBrickU16 {
        scale_level: 0,
        brick_index: SpatialBrickIndex::new(0, 0, 0),
        chunk_index: mirante4d_format::BrickIndex {
            t: 0,
            z: 0,
            y: 0,
            x: 0,
        },
        region: VolumeRegion::new(0, 0, 0, shape.z, shape.y, shape.x).unwrap(),
        occupied,
        valid_voxel_count,
        min: f64::from(value),
        max: f64::from(value),
        volume: volume.clone(),
    };
    (
        volume,
        ResidentBrickSetU16::new(
            layer_id,
            TimeIndex(0),
            shape,
            GridToWorld::identity(),
            vec![brick],
        ),
        shape,
        Shape3D::new(1, 1, 1).unwrap(),
    )
}

fn assert_gpu_brick_frame_matches_cpu_except_skip(
    gpu: Option<&BrickFrameDiagnostics>,
    cpu: &BrickFrameDiagnostics,
    label: &str,
) {
    let gpu = gpu.unwrap_or_else(|| panic!("{label} GPU brick diagnostics are missing"));
    assert_eq!(
        gpu.frame, cpu.frame,
        "{label} brick frame diagnostics differ"
    );
    assert_eq!(
        gpu.complete, cpu.complete,
        "{label} brick completeness differs"
    );
    assert_eq!(
        gpu.missing_voxel_samples, cpu.missing_voxel_samples,
        "{label} missing voxel samples differ"
    );
}

fn assert_gpu_timings_if_enabled(
    renderer: &GpuRenderer,
    timings: Option<GpuRenderTimings>,
    label: &str,
) {
    assert_gpu_timings_for_adapter_if_enabled(renderer.adapter_diagnostics(), timings, label);
}

fn assert_gpu_timings_for_adapter_if_enabled(
    adapter: &AdapterDiagnostics,
    timings: Option<GpuRenderTimings>,
    label: &str,
) {
    if adapter.timestamp_queries_enabled {
        assert!(
            timings.and_then(|timings| timings.gpu_compute_ns).is_some(),
            "{label} GPU timestamp-enabled render must report GPU compute time"
        );
    }
}

#[test]
fn gpu_dense_upload_samples_mark_render_invalid_samples() {
    let u16_volume = DenseVolumeU16::new(
        DatasetId::new("gpu-masked-upload").unwrap(),
        LayerId::new("ch0").unwrap(),
        0,
        TimeIndex(0),
        Shape3D::new(2, 1, 1).unwrap(),
        GridToWorld::identity(),
        vec![u16::MAX, 7],
    )
    .unwrap()
    .with_render_valid(vec![0, 1])
    .unwrap();
    let f32_volume = DenseVolumeF32::new(
        DatasetId::new("gpu-masked-upload-f32").unwrap(),
        LayerId::new("ch0").unwrap(),
        0,
        TimeIndex(0),
        Shape3D::new(2, 1, 1).unwrap(),
        GridToWorld::identity(),
        vec![255.0, 1.5],
    )
    .unwrap()
    .with_render_valid(vec![0, 1])
    .unwrap();

    assert_eq!(
        render_upload_samples_u16(&u16_volume).as_ref(),
        &[GPU_SAMPLE_INVALID_FLAG, 7]
    );
    let f32_samples = render_upload_values_f32(&f32_volume);
    assert!(f32_samples[0].is_nan());
    assert_eq!(f32_samples[1], 1.5);
}

#[test]
fn gpu_brick_atlas_uses_validity_bits_for_render_invalid_samples() {
    let layer_id = LayerId::new("ch0").unwrap();
    let volume = DenseVolumeU16::new(
        DatasetId::new("gpu-masked-atlas").unwrap(),
        layer_id.clone(),
        0,
        TimeIndex(0),
        Shape3D::new(1, 1, 2).unwrap(),
        GridToWorld::identity(),
        vec![u16::MAX, 4],
    )
    .unwrap()
    .with_render_valid(vec![0, 1])
    .unwrap();
    let brick = mirante4d_data::VolumeBrickU16 {
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
        valid_voxel_count: 1,
        min: 4.0,
        max: 4.0,
        volume,
    };
    let resident = ResidentBrickSetU16::new(
        layer_id,
        TimeIndex(0),
        Shape3D::new(1, 1, 2).unwrap(),
        GridToWorld::identity(),
        vec![brick],
    );

    let atlas = build_gpu_brick_atlas(
        &resident,
        Shape3D::new(1, 1, 2).unwrap(),
        Shape3D::new(1, 1, 1).unwrap(),
    )
    .unwrap();

    assert_eq!(atlas.packed_values, vec![4 << 16]);
    assert_eq!(atlas.validity_bits, vec![0b10]);
}

#[test]
fn gpu_brick_atlas_preserves_valid_u16_max_sample() {
    let layer_id = LayerId::new("ch0").unwrap();
    let volume = DenseVolumeU16::new(
        DatasetId::new("gpu-valid-max-atlas").unwrap(),
        layer_id.clone(),
        0,
        TimeIndex(0),
        Shape3D::new(1, 1, 2).unwrap(),
        GridToWorld::identity(),
        vec![u16::MAX, 4],
    )
    .unwrap()
    .with_render_valid(vec![1, 1])
    .unwrap();
    let brick = mirante4d_data::VolumeBrickU16 {
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
        min: 4.0,
        max: f64::from(u16::MAX),
        volume,
    };
    let resident = ResidentBrickSetU16::new(
        layer_id,
        TimeIndex(0),
        Shape3D::new(1, 1, 2).unwrap(),
        GridToWorld::identity(),
        vec![brick],
    );

    let atlas = build_gpu_brick_atlas(
        &resident,
        Shape3D::new(1, 1, 2).unwrap(),
        Shape3D::new(1, 1, 1).unwrap(),
    )
    .unwrap();

    assert_eq!(atlas.packed_values, vec![(4 << 16) | u32::from(u16::MAX)]);
    assert_eq!(atlas.validity_bits, vec![0b11]);
}

#[test]
fn builds_brick_atlas_and_page_table_for_resident_bricks() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let brick = dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex(0),
            mirante4d_data::SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let resident = ResidentBrickSetU16::new(
        layer_id,
        TimeIndex(0),
        Shape3D::new(16, 16, 16).unwrap(),
        brick.volume.grid_to_world,
        vec![brick],
    );

    let atlas = build_gpu_brick_atlas(
        &resident,
        Shape3D::new(16, 16, 16).unwrap(),
        Shape3D::new(1, 1, 1).unwrap(),
    )
    .unwrap();

    assert_eq!(atlas.page_table, vec![1]);
    assert_eq!(atlas.brick_voxel_count, 4096);
    assert_eq!(atlas.packed_values.len(), 2048);
    assert_eq!(atlas.validity_bits.len(), 128);
    assert_eq!(atlas.packed_values[0], 1 << 16);
    assert_eq!(atlas.validity_bits[0], u32::MAX);
}

#[test]
fn brick_atlas_pads_each_slot_for_odd_voxel_bricks() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = tempdir.path().join("odd-bricks.m4d");
    write_native_u16_dataset(
        &root,
        NativeU16Dataset {
            id: "odd-bricks".to_owned(),
            name: "Odd brick fixture".to_owned(),
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
                shape: Shape4D::new(1, 1, 1, 2).unwrap(),
                brick_shape: Shape4D::new(1, 1, 1, 1).unwrap(),
                grid_to_world: GridToWorld::scale_um(1.0, 1.0, 1.0),
                display: default_u16_display(),
                values_tzyx: vec![7, 11],
            }],
        },
        ExistingPackagePolicy::Fail,
    )
    .unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let left = dataset
        .read_u16_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 0))
        .unwrap();
    let right = dataset
        .read_u16_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 1))
        .unwrap();
    let resident = ResidentBrickSetU16::new(
        layer_id,
        TimeIndex(0),
        Shape3D::new(1, 1, 2).unwrap(),
        left.volume.grid_to_world,
        vec![left, right],
    );

    let atlas = build_gpu_brick_atlas(
        &resident,
        Shape3D::new(1, 1, 1).unwrap(),
        Shape3D::new(1, 1, 2).unwrap(),
    )
    .unwrap();

    assert_eq!(atlas.packed_u32_per_brick, 1);
    assert_eq!(atlas.packed_values, vec![7, 11]);
    assert_eq!(atlas.validity_bits, vec![1, 1]);
    assert_eq!(atlas.page_table, vec![1, 2]);
}

#[test]
fn uint8_brick_atlas_packs_four_values_per_word_with_validity_bits() {
    let layer_id = LayerId::new("ch0").unwrap();
    let volume = DenseVolumeU8::new(
        DatasetId::new("gpu-u8-atlas").unwrap(),
        layer_id.clone(),
        0,
        TimeIndex(0),
        Shape3D::new(1, 1, 4).unwrap(),
        GridToWorld::identity(),
        vec![255, 2, 3, 4],
    )
    .unwrap()
    .with_render_valid(vec![1, 0, 1, 1])
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
        region: VolumeRegion::new(0, 0, 0, 1, 1, 4).unwrap(),
        occupied: true,
        valid_voxel_count: 3,
        min: 3.0,
        max: 255.0,
        volume,
    };
    let resident = ResidentBrickSetU8::new(
        layer_id,
        TimeIndex(0),
        Shape3D::new(1, 1, 4).unwrap(),
        GridToWorld::identity(),
        vec![brick],
    );

    let atlas = build_gpu_brick_atlas_u8(
        &resident,
        Shape3D::new(1, 1, 4).unwrap(),
        Shape3D::new(1, 1, 1).unwrap(),
    )
    .unwrap();

    assert_eq!(atlas.packed_u32_per_brick, 1);
    assert_eq!(atlas.valid_u32_per_brick, 1);
    assert_eq!(atlas.packed_values, vec![0x0403_00ff]);
    assert_eq!(atlas.validity_bits, vec![0b1101]);
    assert_eq!(atlas.page_table, vec![1]);
}

#[test]
fn float32_brick_atlas_packs_source_values_without_quantization() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_brick_f32_gpu_fixture(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let brick = dataset
        .read_f32_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 1))
        .unwrap();

    let packed = pack_brick_f32_for_slot(&brick, Shape3D::new(2, 2, 2).unwrap(), 8).unwrap();

    assert_eq!(packed.len(), 8);
    assert_eq!(packed, brick.values());
    assert!(packed.iter().any(|value| *value < 0.0));
    assert!(packed.iter().any(|value| value.fract() != 0.0));
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_renderer_rejects_existing_default_limit_device() {
    let instance = wgpu::Instance::default();
    let default_limits = wgpu::Limits::default();
    let adapter = pollster::block_on(async {
        instance
            .enumerate_adapters(wgpu::Backends::PRIMARY | wgpu::Backends::GL)
            .await
            .into_iter()
            .filter(|adapter| {
                let info = adapter.get_info();
                info.device_type != wgpu::DeviceType::Cpu
                    && info.backend != wgpu::Backend::Noop
                    && adapter.limits().max_storage_buffers_per_shader_stage
                        >= default_limits.max_storage_buffers_per_shader_stage
                    && renderer_required_limits_for_adapter(adapter).is_ok()
            })
            .max_by_key(|adapter| adapter_preference_score(&adapter.get_info()))
    })
    .expect("requires a usable non-CPU GPU adapter that supports WGPU default and renderer limits");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("mirante4d-existing-default-limit-rejected-test-device"),
        required_features: wgpu::Features::empty(),
        required_limits: default_limits,
        experimental_features: wgpu::ExperimentalFeatures::disabled(),
        memory_hints: wgpu::MemoryHints::Performance,
        trace: wgpu::Trace::Off,
    }))
    .unwrap();

    let result = GpuRenderer::from_existing_device_with_cache_budgets(
        &adapter,
        device,
        queue,
        64 * 1024 * 1024,
        64 * 1024 * 1024,
    );

    match result {
        Err(GpuRenderError::DeviceLimitTooLow { limit, .. }) => {
            assert_eq!(limit, "max_storage_buffer_binding_size");
        }
        Err(other) => panic!("expected default-limit device rejection, got {other}"),
        Ok(_) => panic!("default-limit existing device must not satisfy renderer limits"),
    }
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_renderer_constructs_from_existing_renderer_limit_device() {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(async {
        instance
            .enumerate_adapters(wgpu::Backends::PRIMARY | wgpu::Backends::GL)
            .await
            .into_iter()
            .filter(|adapter| {
                let info = adapter.get_info();
                info.device_type != wgpu::DeviceType::Cpu
                    && info.backend != wgpu::Backend::Noop
                    && renderer_required_limits_for_adapter(adapter).is_ok()
            })
            .max_by_key(|adapter| adapter_preference_score(&adapter.get_info()))
    })
    .expect("requires a usable non-CPU GPU adapter that supports renderer limits");
    let descriptor =
        renderer_device_descriptor(&adapter, "mirante4d-existing-renderer-limit-test-device")
            .unwrap();
    let (device, queue) = pollster::block_on(adapter.request_device(&descriptor)).unwrap();

    let renderer = GpuRenderer::from_existing_device_with_cache_budgets(
        &adapter,
        device,
        queue,
        64 * 1024 * 1024,
        64 * 1024 * 1024,
    )
    .unwrap();

    let requested = &renderer.adapter_diagnostics().requested_limits;
    assert!(requested.max_buffer_size >= REQUIRED_MAX_BUFFER_SIZE);
    assert!(requested.max_storage_buffer_binding_size >= REQUIRED_MAX_STORAGE_BUFFER_BINDING_SIZE);
    assert!(
        requested.max_storage_buffers_per_shader_stage
            >= REQUIRED_MAX_STORAGE_BUFFERS_PER_SHADER_STAGE
    );
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_mip_matches_cpu_fixture() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0)).unwrap();
    let (cpu_image, cpu_frame) = render_mip_z(&volume).unwrap();

    let gpu_output = render_mip_z_wgpu_blocking(&volume).unwrap();

    eprintln!(
        "adapter={} backend={} type={} driver={} {}",
        gpu_output.adapter.name,
        gpu_output.adapter.backend,
        gpu_output.adapter.device_type,
        gpu_output.adapter.driver,
        gpu_output.adapter.driver_info
    );
    assert_eq!(gpu_output.image.pixels(), cpu_image.pixels());
    assert_eq!(gpu_output.frame.input_voxels, cpu_frame.input_voxels);
    assert_eq!(gpu_output.frame.output_pixels, cpu_frame.output_pixels);
    assert_eq!(gpu_output.frame.nonzero_pixels, cpu_frame.nonzero_pixels);
    assert_eq!(gpu_output.frame.max_value, cpu_frame.max_value);
    assert_gpu_timings_for_adapter_if_enabled(
        &gpu_output.adapter,
        gpu_output.timings,
        "standalone z-MIP",
    );
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_camera_volume_modes_match_cpu_camera_and_preserve_orthographic_invariants() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0)).unwrap();
    let viewport = RenderViewport::new(16, 16).unwrap();
    let renderer = GpuRenderer::new_blocking().unwrap();

    let front_camera = front_orthographic_camera(&volume, 16.0);
    let side_camera = side_orthographic_camera(&volume, 16.0);
    let far_front_camera = far_front_orthographic_camera(&volume, 16.0);

    let (cpu_front, cpu_front_frame) = render_camera_mip(&volume, front_camera, viewport).unwrap();
    let (cpu_side, _) = render_camera_mip(&volume, side_camera, viewport).unwrap();
    let (cpu_iso, _) = render_camera(
        &volume,
        front_camera,
        viewport,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_threshold(3_000),
        },
    )
    .unwrap();
    let (cpu_dvr, _) = render_camera(
        &volume,
        front_camera,
        viewport,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
        },
    )
    .unwrap();
    let gpu_front = renderer
        .render_camera_mip(&volume, front_camera, viewport)
        .unwrap();
    let gpu_side = renderer
        .render_camera_mip(&volume, side_camera, viewport)
        .unwrap();
    let gpu_far_front = renderer
        .render_camera_mip(&volume, far_front_camera, viewport)
        .unwrap();
    let gpu_iso = renderer
        .render_camera(
            &volume,
            front_camera,
            viewport,
            CameraRenderMode::Isosurface {
                parameters: iso_u16_threshold(3_000),
            },
        )
        .unwrap();
    let gpu_dvr = renderer
        .render_camera(
            &volume,
            front_camera,
            viewport,
            CameraRenderMode::Dvr {
                parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
            },
        )
        .unwrap();
    eprintln!(
        "adapter={} backend={} type={} driver={} {}",
        renderer.adapter_diagnostics().name,
        renderer.adapter_diagnostics().backend,
        renderer.adapter_diagnostics().device_type,
        renderer.adapter_diagnostics().driver,
        renderer.adapter_diagnostics().driver_info
    );
    assert_eq!(gpu_front.image.pixels(), cpu_front.pixels());
    assert_eq!(gpu_front.frame, cpu_front_frame);
    assert_eq!(gpu_side.image.pixels(), cpu_side.pixels());
    assert_ne!(gpu_side.image.pixels(), gpu_front.image.pixels());
    assert_eq!(gpu_far_front.image.pixels(), gpu_front.image.pixels());
    assert_eq!(gpu_iso.image.pixels(), cpu_iso.pixels());
    let gpu_iso_surface = gpu_iso
        .image
        .iso_surface()
        .expect("GPU ISO should include a surface frame");
    let cpu_iso_surface = cpu_iso
        .iso_surface()
        .expect("CPU ISO should include a surface frame");
    assert_eq!(
        gpu_iso_surface.source_values(),
        cpu_iso_surface.source_values()
    );
    assert_eq!(
        gpu_iso_surface.display_scalars(),
        cpu_iso_surface.display_scalars()
    );
    assert_eq!(
        gpu_iso_surface.material_scalars(),
        cpu_iso_surface.material_scalars()
    );
    for (gpu_depth, cpu_depth) in gpu_iso_surface
        .hit_depth()
        .iter()
        .zip(cpu_iso_surface.hit_depth())
    {
        assert!((*gpu_depth - *cpu_depth).abs() <= 1.0e-4);
    }
    assert_eq!(gpu_iso_surface.normals(), cpu_iso_surface.normals());
    assert_eq!(
        gpu_iso_surface.diffuse_lighting(),
        cpu_iso_surface.diffuse_lighting()
    );
    assert_eq!(
        gpu_iso_surface.specular_lighting(),
        cpu_iso_surface.specular_lighting()
    );
    assert_pixels_abs_diff_le(gpu_dvr.image.pixels(), cpu_dvr.pixels(), 1);
    assert_dvr_rgba_abs_diff_le(
        gpu_dvr.image.dvr_rgba(),
        cpu_dvr.dvr_rgba(),
        1.0e-5,
        "dense exact DVR",
    );
    assert_gpu_timings_if_enabled(&renderer, gpu_front.timings, "dense camera");

    let stats = renderer.stats().unwrap();
    assert_eq!(stats.volume_cache_misses, 1);
    assert_eq!(stats.volume_uploads, 1);
    assert_eq!(stats.volume_cache_hits, 4);
    assert_eq!(stats.volume_evictions, 0);
    assert_eq!(
        stats.volume_resident_bytes,
        (volume.values().len() * std::mem::size_of::<u32>()) as u64
    );
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_camera_smooth_quality_matches_cpu_camera() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0)).unwrap();
    let viewport = RenderViewport::new(16, 16).unwrap();
    let camera = front_orthographic_camera(&volume, 16.0);
    let modes = [
        ("mip", CameraRenderMode::Mip),
        (
            "iso",
            CameraRenderMode::Isosurface {
                parameters: iso_u16_threshold(3_000),
            },
        ),
        (
            "dvr",
            CameraRenderMode::Dvr {
                parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
            },
        ),
    ];
    let quality = CameraRenderQuality::smooth_linear();
    let renderer = GpuRenderer::new_blocking().unwrap();

    for (label, mode) in modes {
        let (cpu, cpu_frame) =
            render_camera_with_quality(&volume, camera, viewport, mode, quality).unwrap();
        let gpu = renderer
            .render_camera_with_quality(&volume, camera, viewport, mode, quality)
            .unwrap();

        assert_pixels_abs_diff_le(gpu.image.pixels(), cpu.pixels(), 1);
        if matches!(mode, CameraRenderMode::Dvr { .. }) {
            assert_dvr_rgba_abs_diff_le(gpu.image.dvr_rgba(), cpu.dvr_rgba(), 1.0e-5, label);
        }
        if matches!(mode, CameraRenderMode::Isosurface { .. }) {
            assert!(
                gpu.image.iso_surface().is_some(),
                "{label} GPU ISO should include a surface frame"
            );
            assert!(
                cpu.iso_surface().is_some(),
                "{label} CPU ISO should include a surface frame"
            );
        }
        assert_eq!(
            gpu.frame.output_pixels, cpu_frame.output_pixels,
            "{label} output pixel count differs"
        );
        assert_eq!(
            gpu.frame.nonzero_pixels, cpu_frame.nonzero_pixels,
            "{label} nonzero pixel count differs"
        );
    }
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_bricked_camera_modes_match_cpu_resident_bricks() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0)).unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();
    let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
    let brick = dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex(0),
            mirante4d_data::SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let resident = ResidentBrickSetU16::new(
        layer_id,
        TimeIndex(0),
        volume.shape,
        volume.grid_to_world,
        vec![brick],
    );
    let viewport = RenderViewport::new(16, 16).unwrap();
    let camera = front_orthographic_camera(&volume, 16.0);
    let modes = [
        ("mip", CameraRenderMode::Mip),
        (
            "iso",
            CameraRenderMode::Isosurface {
                parameters: iso_u16_threshold(3_000),
            },
        ),
        (
            "dvr",
            CameraRenderMode::Dvr {
                parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
            },
        ),
    ];
    let renderer = GpuRenderer::new_blocking().unwrap();

    for (label, mode) in modes {
        let (cpu, cpu_diagnostics) =
            crate::render_camera_from_bricks(&resident, camera, viewport, mode).unwrap();
        let gpu = renderer
            .render_camera_from_bricks(
                &resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                mode,
            )
            .unwrap();

        assert!(
            cpu_diagnostics.complete,
            "{label} CPU resident-brick reference must be complete"
        );
        if matches!(mode, CameraRenderMode::Dvr { .. }) {
            assert_pixels_abs_diff_le(gpu.image.pixels(), cpu.pixels(), 1);
            assert_dvr_rgba_abs_diff_le(gpu.image.dvr_rgba(), cpu.dvr_rgba(), 1.0e-5, label);
        } else {
            assert_eq!(gpu.image.pixels(), cpu.pixels(), "{label} pixels differ");
        }
        assert_eq!(
            gpu.frame, cpu_diagnostics.frame,
            "{label} diagnostics differ"
        );
        assert_gpu_brick_frame_matches_cpu_except_skip(
            gpu.brick_frame.as_ref(),
            &cpu_diagnostics,
            label,
        );
    }

    eprintln!(
        "adapter={} backend={} type={} driver={} {}",
        renderer.adapter_diagnostics().name,
        renderer.adapter_diagnostics().backend,
        renderer.adapter_diagnostics().device_type,
        renderer.adapter_diagnostics().driver,
        renderer.adapter_diagnostics().driver_info
    );
    let stats = renderer.stats().unwrap();
    assert_eq!(stats.brick_atlas_cache_misses, 1);
    assert_eq!(stats.brick_atlas_uploads, 1);
    assert_eq!(stats.brick_atlas_cache_hits, 2);
    assert_eq!(stats.brick_atlas_evictions, 0);
    assert!(stats.brick_atlas_resident_bytes > 0);
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_bricked_u8_camera_modes_match_cpu_resident_bricks() {
    let layer_id = LayerId::new("ch0").unwrap();
    let shape = Shape3D::new(4, 4, 4).unwrap();
    let grid_to_world = GridToWorld::identity();
    let values = (0..shape.element_count().unwrap())
        .map(|index| u8::try_from(index).unwrap())
        .collect::<Vec<_>>();
    let volume = DenseVolumeU8::new(
        DatasetId::new("gpu-u8-resident").unwrap(),
        layer_id.clone(),
        0,
        TimeIndex(0),
        shape,
        grid_to_world,
        values,
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
        region: VolumeRegion::new(0, 0, 0, 4, 4, 4).unwrap(),
        occupied: true,
        valid_voxel_count: shape.element_count().unwrap(),
        min: 0.0,
        max: 63.0,
        volume,
    };
    let resident =
        ResidentBrickSetU8::new(layer_id, TimeIndex(0), shape, grid_to_world, vec![brick]);
    let camera_volume = DenseVolumeU16::new(
        DatasetId::new("gpu-u8-camera").unwrap(),
        LayerId::new("camera").unwrap(),
        0,
        TimeIndex(0),
        shape,
        grid_to_world,
        vec![0; shape.element_count().unwrap() as usize],
    )
    .unwrap();
    let camera = front_orthographic_camera(&camera_volume, 4.0);
    let viewport = RenderViewport::new(4, 4).unwrap();
    let modes = [
        ("mip", CameraRenderMode::Mip),
        (
            "iso",
            CameraRenderMode::Isosurface {
                parameters: iso_u16_threshold(30),
            },
        ),
        (
            "dvr",
            CameraRenderMode::Dvr {
                parameters: dvr_parameters(0.0, f32::from(u8::MAX), 12.0, false),
            },
        ),
    ];
    let renderer = GpuRenderer::new_blocking().unwrap();

    for (label, mode) in modes {
        let (cpu, cpu_diagnostics) = crate::render_camera_u8_from_bricks_with_quality(
            &resident,
            camera,
            viewport,
            mode,
            CameraRenderQuality::voxel_exact(),
        )
        .unwrap();
        let gpu = renderer
            .render_camera_u8_from_bricks_with_quality(
                &resident,
                shape,
                Shape3D::new(1, 1, 1).unwrap(),
                camera,
                viewport,
                mode,
                CameraRenderQuality::voxel_exact(),
            )
            .unwrap();

        assert!(
            cpu_diagnostics.complete,
            "{label} CPU u8 resident-brick reference must be complete"
        );
        if matches!(mode, CameraRenderMode::Dvr { .. }) {
            assert_pixels_abs_diff_le(gpu.image.pixels(), cpu.pixels(), 1);
            assert_dvr_rgba_abs_diff_le(gpu.image.dvr_rgba(), cpu.dvr_rgba(), 1.0e-5, label);
        } else {
            assert_eq!(gpu.image.pixels(), cpu.pixels(), "{label} pixels differ");
        }
        assert_gpu_brick_frame_matches_cpu_except_skip(
            gpu.brick_frame.as_ref(),
            &cpu_diagnostics,
            label,
        );
    }

    let stats = renderer.stats().unwrap();
    let brick_voxel_count = shape.element_count().unwrap();
    let minimum_slot_count = 1;
    let minimum_resident_bytes = minimum_slot_count
        * (brick_voxel_count.div_ceil(4) + brick_voxel_count.div_ceil(32))
        * std::mem::size_of::<u32>() as u64
        + std::mem::size_of::<u32>() as u64
        + 4 * std::mem::size_of::<u32>() as u64;
    assert!(stats.brick_atlas_u8_resident_bytes >= minimum_resident_bytes);
    assert!(stats.brick_atlas_u8_resident_bytes <= stats.brick_atlas_cache_budget_bytes);
    assert_eq!(stats.brick_atlas_u16_resident_bytes, 0);
    assert_eq!(stats.brick_atlas_f32_resident_bytes, 0);
    assert_eq!(stats.brick_atlas_u8_uploaded_bytes, 72);
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_bricked_camera_reports_metadata_skip_diagnostics() {
    let renderer = GpuRenderer::new_blocking().unwrap();
    let viewport = RenderViewport::new(4, 4).unwrap();

    let render_skip = |layer: &str, value: u16, occupied: bool, mode: CameraRenderMode| {
        let (volume, resident, brick_shape, brick_grid_shape) =
            resident_u16_constant_brick(layer, value, occupied);
        let camera = front_orthographic_camera(&volume, 4.0);
        let output = renderer
            .render_camera_from_bricks(
                &resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                mode,
            )
            .unwrap();
        assert_gpu_timings_if_enabled(&renderer, output.timings, layer);
        let diagnostics = output
            .brick_frame
            .expect("GPU resident-brick render must return diagnostics");
        assert!(diagnostics.complete, "{layer} render should be complete");
        assert_eq!(diagnostics.missing_voxel_samples, 0);
        diagnostics.skip
    };

    let empty = render_skip("skip-empty", 0, false, CameraRenderMode::Mip);
    assert!(empty.empty_brick_intervals > 0);
    assert_eq!(empty.skipped_brick_intervals, empty.empty_brick_intervals);

    let mip = render_skip("skip-mip", 0, true, CameraRenderMode::Mip);
    assert!(mip.mip_range_intervals > 0);
    assert_eq!(mip.skipped_brick_intervals, mip.mip_range_intervals);

    let iso = render_skip(
        "skip-iso",
        0,
        true,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_threshold(1),
        },
    );
    assert!(iso.iso_range_intervals > 0);
    assert_eq!(iso.skipped_brick_intervals, iso.iso_range_intervals);

    let dvr = render_skip(
        "skip-dvr",
        0,
        true,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(1.0, 2.0, 12.0, false),
        },
    );
    assert!(dvr.dvr_range_intervals > 0);
    assert_eq!(dvr.skipped_brick_intervals, dvr.dvr_range_intervals);

    let smooth_quality = CameraRenderQuality::smooth_linear();
    let render_smooth_skip = |layer: &str, value: u16, occupied: bool, mode: CameraRenderMode| {
        let (volume, resident, brick_shape, brick_grid_shape) =
            resident_u16_constant_brick(layer, value, occupied);
        let camera = front_orthographic_camera(&volume, 4.0);
        let output = renderer
            .render_camera_from_bricks_with_quality(
                &resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                mode,
                smooth_quality,
            )
            .unwrap();
        assert_gpu_timings_if_enabled(&renderer, output.timings, layer);
        let diagnostics = output
            .brick_frame
            .expect("GPU resident-brick render must return diagnostics");
        assert!(diagnostics.complete, "{layer} render should be complete");
        assert_eq!(diagnostics.missing_voxel_samples, 0);
        diagnostics.skip
    };

    let smooth_empty = render_smooth_skip("smooth-skip-empty", 0, false, CameraRenderMode::Mip);
    assert!(smooth_empty.empty_brick_intervals > 0);
    assert_eq!(
        smooth_empty.skipped_brick_intervals,
        smooth_empty.empty_brick_intervals
    );

    let smooth_mip = render_smooth_skip("smooth-skip-mip", 0, true, CameraRenderMode::Mip);
    assert!(smooth_mip.mip_range_intervals > 0);
    assert_eq!(
        smooth_mip.skipped_brick_intervals,
        smooth_mip.mip_range_intervals
    );

    let smooth_iso = render_smooth_skip(
        "smooth-skip-iso",
        0,
        true,
        CameraRenderMode::Isosurface {
            parameters: iso_u16_threshold(1),
        },
    );
    assert_eq!(smooth_iso.skipped_brick_intervals, 0);

    let smooth_dvr = render_smooth_skip(
        "smooth-skip-dvr",
        0,
        true,
        CameraRenderMode::Dvr {
            parameters: dvr_parameters(1.0, 2.0, 12.0, false),
        },
    );
    assert!(smooth_dvr.dvr_range_intervals > 0);
    assert_eq!(
        smooth_dvr.skipped_brick_intervals,
        smooth_dvr.dvr_range_intervals
    );
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_bricked_smooth_quality_matches_cpu_resident_bricks() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0)).unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();
    let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
    let brick = dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex(0),
            mirante4d_data::SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let resident = ResidentBrickSetU16::new(
        layer_id,
        TimeIndex(0),
        volume.shape,
        volume.grid_to_world,
        vec![brick],
    );
    let viewport = RenderViewport::new(16, 16).unwrap();
    let camera = front_orthographic_camera(&volume, 16.0);
    let modes = [
        ("mip", CameraRenderMode::Mip),
        (
            "iso",
            CameraRenderMode::Isosurface {
                parameters: iso_u16_threshold(3_000),
            },
        ),
        (
            "dvr",
            CameraRenderMode::Dvr {
                parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
            },
        ),
    ];
    let quality = CameraRenderQuality::smooth_linear();
    let renderer = GpuRenderer::new_blocking().unwrap();

    for (label, mode) in modes {
        let (cpu, cpu_diagnostics) = crate::render_camera_from_bricks_with_quality(
            &resident, camera, viewport, mode, quality,
        )
        .unwrap();
        let gpu = renderer
            .render_camera_from_bricks_with_quality(
                &resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                mode,
                quality,
            )
            .unwrap();

        assert!(
            cpu_diagnostics.complete,
            "{label} CPU resident-brick reference must be complete"
        );
        assert_pixels_abs_diff_le(gpu.image.pixels(), cpu.pixels(), 1);
        if matches!(mode, CameraRenderMode::Dvr { .. }) {
            assert_dvr_rgba_abs_diff_le(gpu.image.dvr_rgba(), cpu.dvr_rgba(), 1.0e-5, label);
        }
        assert_gpu_brick_frame_matches_cpu_except_skip(
            gpu.brick_frame.as_ref(),
            &cpu_diagnostics,
            label,
        );
    }
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_batched_bricked_mip_matches_monolithic_gpu_mip() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_brick_gpu_fixture(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0)).unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();
    let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
    let bricks = (0..3)
        .map(|x| {
            dataset
                .read_u16_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, x))
                .unwrap()
        })
        .collect::<Vec<_>>();
    let resident = ResidentBrickSetU16::new(
        layer_id,
        TimeIndex(0),
        volume.shape,
        volume.grid_to_world,
        bricks,
    );
    let viewport = RenderViewport::new(6, 2).unwrap();
    let camera = front_orthographic_camera(&volume, 2.0);
    let renderer = GpuRenderer::new_blocking().unwrap();

    let monolithic = renderer
        .render_camera_from_bricks(
            &resident,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
            CameraRenderMode::Mip,
        )
        .unwrap();
    let batched = renderer
        .render_camera_mip_from_bricks_batched(
            &resident,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
            1,
        )
        .unwrap();

    assert_eq!(batched.image.pixels(), monolithic.image.pixels());
    assert_eq!(batched.frame, monolithic.frame);
    assert!(batched.brick_frame.unwrap().complete);
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_bricked_camera_modes_report_incomplete_residency() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_brick_gpu_fixture(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0)).unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();
    let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
    let brick_0 = dataset
        .read_u16_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 0))
        .unwrap();
    let resident = ResidentBrickSetU16::new(
        layer_id,
        TimeIndex(0),
        volume.shape,
        volume.grid_to_world,
        vec![brick_0],
    );
    let viewport = RenderViewport::new(6, 2).unwrap();
    let camera = front_orthographic_camera(&volume, 2.0);
    let modes = [
        ("mip", CameraRenderMode::Mip),
        (
            "iso",
            CameraRenderMode::Isosurface {
                parameters: iso_u16_threshold(1_000),
            },
        ),
        (
            "dvr",
            CameraRenderMode::Dvr {
                parameters: dvr_parameters(0.0, f32::from(u16::MAX), 12.0, false),
            },
        ),
    ];
    let renderer = GpuRenderer::new_blocking().unwrap();

    for (label, mode) in modes {
        let (cpu, cpu_diagnostics) =
            crate::render_camera_from_bricks(&resident, camera, viewport, mode).unwrap();
        let gpu = renderer
            .render_camera_from_bricks(
                &resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                mode,
            )
            .unwrap();
        assert!(
            !cpu_diagnostics.complete,
            "{label} CPU reference should report incomplete residency"
        );
        assert!(
            cpu_diagnostics.missing_voxel_samples > 0,
            "{label} CPU reference should count missing samples"
        );
        if matches!(mode, CameraRenderMode::Dvr { .. }) {
            assert_pixels_abs_diff_le(gpu.image.pixels(), cpu.pixels(), 1);
            assert_dvr_rgba_abs_diff_le(gpu.image.dvr_rgba(), cpu.dvr_rgba(), 1.0e-5, label);
        } else {
            assert_eq!(gpu.image.pixels(), cpu.pixels(), "{label} pixels differ");
        }
        assert_gpu_brick_frame_matches_cpu_except_skip(
            gpu.brick_frame.as_ref(),
            &cpu_diagnostics,
            label,
        );
    }
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_brick_atlas_reuses_overlapping_pages_between_resident_sets() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_brick_gpu_fixture(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_u16_volume(&layer_id, TimeIndex(0)).unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();
    let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
    let brick_0 = dataset
        .read_u16_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 0))
        .unwrap();
    let brick_1 = dataset
        .read_u16_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 1))
        .unwrap();
    let brick_2 = dataset
        .read_u16_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 2))
        .unwrap();
    let first = ResidentBrickSetU16::new(
        layer_id.clone(),
        TimeIndex(0),
        volume.shape,
        volume.grid_to_world,
        vec![brick_0, brick_1.clone()],
    );
    let second = ResidentBrickSetU16::new(
        layer_id,
        TimeIndex(0),
        volume.shape,
        volume.grid_to_world,
        vec![brick_1, brick_2],
    );
    let viewport = RenderViewport::new(6, 2).unwrap();
    let camera = front_orthographic_camera(&volume, 2.0);
    let renderer = GpuRenderer::new_blocking().unwrap();

    renderer
        .render_camera_from_bricks(
            &first,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
            CameraRenderMode::Mip,
        )
        .unwrap();
    renderer
        .render_camera_from_bricks(
            &second,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
            CameraRenderMode::Mip,
        )
        .unwrap();

    let stats = renderer.stats().unwrap();
    assert_eq!(stats.brick_atlas_cache_misses, 1);
    assert_eq!(stats.brick_atlas_cache_hits, 1);
    assert_eq!(stats.brick_atlas_uploads, 3);
    assert_eq!(stats.brick_atlas_evictions, 0);

    let key = crate::BrickAtlasResourceKey::from_resident(&second, brick_shape, brick_grid_shape)
        .unwrap();
    let residency = renderer.brick_atlas_residency(&key).unwrap();
    assert!(residency.retained);
    assert_eq!(
        residency.active_pages,
        [
            SpatialBrickIndex::new(0, 0, 1),
            SpatialBrickIndex::new(0, 0, 2)
        ]
        .into_iter()
        .collect()
    );
    assert!(
        residency
            .resident_pages
            .contains(&SpatialBrickIndex::new(0, 0, 0))
    );
    assert!(
        residency
            .resident_pages
            .contains(&SpatialBrickIndex::new(0, 0, 1))
    );
    assert!(
        residency
            .resident_pages
            .contains(&SpatialBrickIndex::new(0, 0, 2))
    );
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_f32_brick_atlas_reuses_overlapping_pages_between_resident_sets() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_brick_f32_gpu_fixture(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_f32_volume(&layer_id, TimeIndex(0)).unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();
    let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
    let brick_0 = dataset
        .read_f32_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 0))
        .unwrap();
    let brick_1 = dataset
        .read_f32_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 1))
        .unwrap();
    let brick_2 = dataset
        .read_f32_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 2))
        .unwrap();
    let first = ResidentBrickSetF32::new(
        layer_id.clone(),
        TimeIndex(0),
        volume.shape,
        volume.grid_to_world,
        vec![brick_0, brick_1.clone()],
    );
    let second = ResidentBrickSetF32::new(
        layer_id,
        TimeIndex(0),
        volume.shape,
        volume.grid_to_world,
        vec![brick_1, brick_2],
    );
    let viewport = RenderViewport::new(6, 2).unwrap();
    let camera = front_orthographic_camera_f32(&volume, 2.0);
    let renderer = GpuRenderer::new_blocking().unwrap();

    renderer
        .render_camera_f32_from_bricks(
            &first,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
            CameraRenderModeF32::Mip,
        )
        .unwrap();
    renderer
        .render_camera_f32_from_bricks(
            &second,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
            CameraRenderModeF32::Mip,
        )
        .unwrap();

    let stats = renderer.stats().unwrap();
    assert_eq!(stats.brick_atlas_cache_misses, 1);
    assert_eq!(stats.brick_atlas_cache_hits, 1);
    assert_eq!(stats.brick_atlas_uploads, 3);
    assert_eq!(stats.brick_atlas_evictions, 0);

    let key =
        crate::BrickAtlasResourceKey::from_resident_f32(&second, brick_shape, brick_grid_shape)
            .unwrap();
    let residency = renderer.brick_atlas_residency(&key).unwrap();
    assert!(residency.retained);
    assert_eq!(
        residency.active_pages,
        [
            SpatialBrickIndex::new(0, 0, 1),
            SpatialBrickIndex::new(0, 0, 2)
        ]
        .into_iter()
        .collect()
    );
    assert!(
        residency
            .resident_pages
            .contains(&SpatialBrickIndex::new(0, 0, 0))
    );
    assert!(
        residency
            .resident_pages
            .contains(&SpatialBrickIndex::new(0, 0, 1))
    );
    assert!(
        residency
            .resident_pages
            .contains(&SpatialBrickIndex::new(0, 0, 2))
    );
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_float32_bricked_camera_modes_match_cpu_resident_bricks() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_brick_f32_gpu_fixture(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_f32_volume(&layer_id, TimeIndex(0)).unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();
    let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
    let bricks = [
        SpatialBrickIndex::new(0, 0, 0),
        SpatialBrickIndex::new(0, 0, 1),
        SpatialBrickIndex::new(0, 0, 2),
    ]
    .into_iter()
    .map(|index| {
        dataset
            .read_f32_brick(&layer_id, TimeIndex(0), index)
            .unwrap()
    })
    .collect::<Vec<_>>();
    let resident = ResidentBrickSetF32::new(
        layer_id,
        TimeIndex(0),
        volume.shape,
        volume.grid_to_world,
        bricks,
    );
    let viewport = RenderViewport::new(6, 2).unwrap();
    let camera = front_orthographic_camera_f32(&volume, 2.0);
    let modes = [
        ("mip", CameraRenderModeF32::Mip),
        (
            "iso",
            CameraRenderModeF32::Isosurface {
                parameters: iso_f32_threshold(1.25, 0.0, 4.0),
            },
        ),
        (
            "dvr",
            CameraRenderModeF32::Dvr {
                parameters: dvr_parameters(-6.0, 6.0, 8.0, false),
            },
        ),
    ];
    let renderer = GpuRenderer::new_blocking().unwrap();

    for (label, mode) in modes {
        let (cpu, cpu_diagnostics) =
            render_camera_f32_from_bricks(&resident, camera, viewport, mode).unwrap();
        let gpu = renderer
            .render_camera_f32_from_bricks(
                &resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                mode,
            )
            .unwrap();

        assert!(
            cpu_diagnostics.complete,
            "{label} CPU f32 resident-brick reference must be complete"
        );
        assert_f32_pixels_abs_diff_le(gpu.image.pixels(), cpu.pixels(), 1.0e-5, label);
        if matches!(mode, CameraRenderModeF32::Dvr { .. }) {
            assert_dvr_rgba_abs_diff_le(gpu.image.dvr_rgba(), cpu.dvr_rgba(), 1.0e-5, label);
        }
        assert_eq!(gpu.frame.input_voxels, cpu_diagnostics.frame.input_voxels);
        assert_eq!(gpu.frame.output_pixels, cpu_diagnostics.frame.output_pixels);
        assert_eq!(
            gpu.frame.nonzero_pixels,
            cpu_diagnostics.frame.nonzero_pixels
        );
        assert!(
            (gpu.frame.max_value - cpu_diagnostics.frame.max_value).abs() <= 1.0e-5,
            "{label} max value differs: gpu={}, cpu={}",
            gpu.frame.max_value,
            cpu_diagnostics.frame.max_value
        );
        let gpu_brick = gpu
            .brick_frame
            .expect("GPU f32 resident-brick renders must return brick diagnostics");
        assert!(
            gpu_brick.complete,
            "{label} GPU f32 render should be complete"
        );
        assert_eq!(gpu_brick.missing_voxel_samples, 0);
    }

    eprintln!(
        "adapter={} backend={} type={} driver={} {}",
        renderer.adapter_diagnostics().name,
        renderer.adapter_diagnostics().backend,
        renderer.adapter_diagnostics().device_type,
        renderer.adapter_diagnostics().driver,
        renderer.adapter_diagnostics().driver_info
    );
    let stats = renderer.stats().unwrap();
    assert_eq!(stats.brick_atlas_cache_misses, 1);
    assert_eq!(stats.brick_atlas_uploads, 3);
    assert_eq!(stats.brick_atlas_cache_hits, 2);
    assert_eq!(stats.brick_atlas_evictions, 0);
    assert!(stats.brick_atlas_resident_bytes > 0);
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_float32_bricked_smooth_quality_matches_cpu_resident_bricks() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_brick_f32_gpu_fixture(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_f32_volume(&layer_id, TimeIndex(0)).unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();
    let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
    let bricks = [
        SpatialBrickIndex::new(0, 0, 0),
        SpatialBrickIndex::new(0, 0, 1),
        SpatialBrickIndex::new(0, 0, 2),
    ]
    .into_iter()
    .map(|index| {
        dataset
            .read_f32_brick(&layer_id, TimeIndex(0), index)
            .unwrap()
    })
    .collect::<Vec<_>>();
    let resident = ResidentBrickSetF32::new(
        layer_id,
        TimeIndex(0),
        volume.shape,
        volume.grid_to_world,
        bricks,
    );
    let viewport = RenderViewport::new(6, 2).unwrap();
    let camera = front_orthographic_camera_f32(&volume, 2.0);
    let modes = [
        ("mip", CameraRenderModeF32::Mip),
        (
            "iso",
            CameraRenderModeF32::Isosurface {
                parameters: iso_f32_threshold(1.25, 0.0, 4.0),
            },
        ),
        (
            "dvr",
            CameraRenderModeF32::Dvr {
                parameters: dvr_parameters(-6.0, 6.0, 8.0, false),
            },
        ),
    ];
    let quality = CameraRenderQuality::smooth_linear();
    let renderer = GpuRenderer::new_blocking().unwrap();

    for (label, mode) in modes {
        let (cpu, cpu_diagnostics) = crate::render_camera_f32_from_bricks_with_quality(
            &resident, camera, viewport, mode, quality,
        )
        .unwrap();
        let gpu = renderer
            .render_camera_f32_from_bricks_with_quality(
                &resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                mode,
                quality,
            )
            .unwrap();
        assert_gpu_timings_if_enabled(&renderer, gpu.timings, label);

        assert!(
            cpu_diagnostics.complete,
            "{label} CPU f32 resident-brick reference must be complete"
        );
        assert_f32_pixels_abs_diff_le(gpu.image.pixels(), cpu.pixels(), 1.0e-5, label);
        if matches!(mode, CameraRenderModeF32::Dvr { .. }) {
            assert_dvr_rgba_abs_diff_le(gpu.image.dvr_rgba(), cpu.dvr_rgba(), 1.0e-5, label);
        }
        let gpu_brick = gpu
            .brick_frame
            .expect("GPU f32 resident-brick renders must return brick diagnostics");
        assert_eq!(gpu_brick.complete, cpu_diagnostics.complete);
        assert_eq!(
            gpu_brick.missing_voxel_samples,
            cpu_diagnostics.missing_voxel_samples
        );
        assert_eq!(
            gpu_brick.frame.input_voxels,
            cpu_diagnostics.frame.input_voxels
        );
        assert_eq!(
            gpu_brick.frame.output_pixels,
            cpu_diagnostics.frame.output_pixels
        );
        assert_eq!(
            gpu_brick.frame.nonzero_pixels,
            cpu_diagnostics.frame.nonzero_pixels
        );
        assert!(
            (gpu_brick.frame.max_value - cpu_diagnostics.frame.max_value).abs() <= 1.0e-5,
            "{label} f32 max value differs: gpu={}, cpu={}",
            gpu_brick.frame.max_value,
            cpu_diagnostics.frame.max_value
        );
    }
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_float32_bricked_camera_modes_report_incomplete_residency() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_brick_f32_gpu_fixture(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset.read_f32_volume(&layer_id, TimeIndex(0)).unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();
    let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
    let brick_0 = dataset
        .read_f32_brick(&layer_id, TimeIndex(0), SpatialBrickIndex::new(0, 0, 0))
        .unwrap();
    let resident = ResidentBrickSetF32::new(
        layer_id,
        TimeIndex(0),
        volume.shape,
        volume.grid_to_world,
        vec![brick_0],
    );
    let viewport = RenderViewport::new(6, 2).unwrap();
    let camera = front_orthographic_camera_f32(&volume, 2.0);
    let modes = [
        ("mip", CameraRenderModeF32::Mip),
        (
            "iso",
            CameraRenderModeF32::Isosurface {
                parameters: iso_f32_threshold(1.25, 0.0, 4.0),
            },
        ),
        (
            "dvr",
            CameraRenderModeF32::Dvr {
                parameters: dvr_parameters(-6.0, 6.0, 8.0, false),
            },
        ),
    ];
    let renderer = GpuRenderer::new_blocking().unwrap();

    for (label, mode) in modes {
        let (cpu, cpu_diagnostics) =
            render_camera_f32_from_bricks(&resident, camera, viewport, mode).unwrap();
        let gpu = renderer
            .render_camera_f32_from_bricks(
                &resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                mode,
            )
            .unwrap();

        assert!(
            !cpu_diagnostics.complete,
            "{label} CPU f32 reference should report incomplete residency"
        );
        assert!(
            cpu_diagnostics.missing_voxel_samples > 0,
            "{label} CPU f32 reference should count missing samples"
        );
        assert_f32_pixels_abs_diff_le(gpu.image.pixels(), cpu.pixels(), 1.0e-5, label);
        if matches!(mode, CameraRenderModeF32::Dvr { .. }) {
            assert_dvr_rgba_abs_diff_le(gpu.image.dvr_rgba(), cpu.dvr_rgba(), 1.0e-5, label);
        }
        assert_eq!(
            gpu.brick_frame,
            Some(cpu_diagnostics),
            "{label} GPU f32 brick diagnostics differ"
        );
    }
}

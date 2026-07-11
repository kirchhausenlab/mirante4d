use std::sync::mpsc;

use glam::DVec3;
use mirante4d_data::{
    DatasetHandle, DenseVolumeF32, DenseVolumeU16, SpatialBrickIndex, VolumeBrickF32,
    VolumeBrickU16, VolumeRegion,
};
use mirante4d_domain::{
    DisplayWindow, GridToWorld, IsoLightState, LayerTransfer, Opacity, RgbColor, TimeIndex,
    TransferCurve,
};
use mirante4d_format::{DatasetId, FixtureKind, LayerId, write_fixture};
use mirante4d_render_api::{CameraAxes, CameraFrame};

use crate::camera_mip::CameraRenderModeF32;
use crate::{
    CameraRenderMode, CameraRenderQuality, CoordinateSpace, DvrRenderParameters,
    DvrResidentChannel, DvrRgbaChannelFrame, IntensityChannelFrame, IntensityChannelFrameF32,
    IntensityTransfer, IsoSurfaceChannelFrame, IsoSurfaceChannelFrameF32, OcclusionPolicy,
    RenderViewport, ResidentBrickSetF32, ResidentBrickSetU16, ScalarDisplayTransfer,
    SceneColorRgba, SceneFrameContext, SceneGeometry, SceneLayer, SceneLayerId, SceneLayerKind,
    SceneObject, SceneObjectId, SceneStyle, SceneTime, composite_dvr_rgba_channels,
    composite_f32_intensity_channels, composite_intensity_channels, composite_iso_surface_channels,
    composite_iso_surface_f32_channels, extract_scene_draw_list, render_camera_f32_from_bricks,
    render_dvr_channels_from_bricks_with_quality,
};

use super::test_support::*;
use super::*;

fn read_display_frame_rgba(
    renderer: &GpuRenderer,
    frame: &GpuDisplayFrame,
) -> Result<Vec<u8>, GpuRenderError> {
    let width = u32::try_from(frame.viewport.width).unwrap();
    let height = u32::try_from(frame.viewport.height).unwrap();
    let unpadded_bytes_per_row = width * 4;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer_size = u64::from(padded_bytes_per_row) * u64::from(height);
    let readback = renderer.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-test-display-texture-readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = renderer
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mirante4d-test-display-texture-readback-encoder"),
        });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: frame.texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let submission = renderer.queue.submit(Some(encoder.finish()));
    let (sender, receiver) = mpsc::channel();
    readback
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
    renderer
        .device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })
        .map_err(|err| GpuRenderError::PollFailed(err.to_string()))?;
    receiver
        .recv()
        .map_err(|_| GpuRenderError::ReadbackChannelClosed)?
        .map_err(|err| GpuRenderError::MapFailed(err.to_string()))?;

    let mapped = readback.slice(..).get_mapped_range();
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for row in 0..height as usize {
        let start = row * padded_bytes_per_row as usize;
        let end = start + unpadded_bytes_per_row as usize;
        rgba.extend_from_slice(&mapped[start..end]);
    }
    drop(mapped);
    readback.unmap();
    Ok(rgba)
}

fn assert_rgba_abs_diff_le(actual: &[u8], expected: &[u8], max_diff: u8) {
    assert_eq!(actual.len(), expected.len());
    for (index, (&actual, &expected)) in actual.iter().zip(expected).enumerate() {
        let diff = actual.abs_diff(expected);
        assert!(
            diff <= max_diff,
            "RGBA byte {index} differs by {diff}: actual={actual}, expected={expected}, max_diff={max_diff}"
        );
    }
}

fn additive_display_frame_channel(base: u8, source: u8, source_alpha: u8) -> u8 {
    ((f32::from(base) / 255.0 + (f32::from(source) / 255.0) * (f32::from(source_alpha) / 255.0))
        .clamp(0.0, 1.0)
        * 255.0)
        .round() as u8
}

fn additive_display_frame_alpha(base_alpha: u8, source_alpha: u8) -> u8 {
    let base_alpha = f32::from(base_alpha) / 255.0;
    let source_alpha = f32::from(source_alpha) / 255.0;
    ((1.0 - (1.0 - base_alpha) * (1.0 - source_alpha)).clamp(0.0, 1.0) * 255.0).round() as u8
}

fn additive_display_frame_reference(base: &[u8], source: &[u8]) -> Vec<u8> {
    assert_eq!(base.len(), source.len());
    let mut blended = Vec::with_capacity(base.len());
    for (base, source) in base.chunks_exact(4).zip(source.chunks_exact(4)) {
        blended.push(additive_display_frame_channel(
            base[0], source[0], source[3],
        ));
        blended.push(additive_display_frame_channel(
            base[1], source[1], source[3],
        ));
        blended.push(additive_display_frame_channel(
            base[2], source[2], source[3],
        ));
        blended.push(additive_display_frame_alpha(base[3], source[3]));
    }
    blended
}

fn assert_gpu_compute_timing_when_enabled(renderer: &GpuRenderer, frame: &GpuDisplayFrame) {
    if renderer.adapter_diagnostics().timestamp_queries_enabled {
        assert!(
            frame.timings.gpu_compute_ns.is_some(),
            "timestamp-enabled GPU display renders must report GPU compute timing"
        );
    }
}

fn display_transfer(color_rgba: [f32; 4], opacity: f32, window_high: f32) -> IntensityTransfer {
    let [red, green, blue, _alpha] = color_rgba;
    IntensityTransfer::new(
        true,
        LayerTransfer::new(
            DisplayWindow::new(0.0, window_high).unwrap(),
            RgbColor::new([red, green, blue]).unwrap(),
            Opacity::new(opacity).unwrap(),
            TransferCurve::linear(),
            false,
        ),
    )
}

fn display_transfer_window(
    color_rgba: [f32; 4],
    opacity: f32,
    low: f32,
    high: f32,
) -> IntensityTransfer {
    let [red, green, blue, _alpha] = color_rgba;
    IntensityTransfer::new(
        true,
        LayerTransfer::new(
            DisplayWindow::new(low, high).unwrap(),
            RgbColor::new([red, green, blue]).unwrap(),
            Opacity::new(opacity).unwrap(),
            TransferCurve::linear(),
            false,
        ),
    )
}

fn dvr_parameters_for_transfer(
    transfer: &IntensityTransfer,
    density_scale: f64,
) -> DvrRenderParameters {
    DvrRenderParameters::new(
        ScalarDisplayTransfer::from_intensity_transfer(*transfer),
        ScalarDisplayTransfer::new(transfer.window(), transfer.curve(), false),
        transfer.color_rgba(),
        transfer.opacity().get(),
        density_scale,
    )
}

fn camera_state_axes(camera: CameraFrame) -> CameraAxes {
    camera.axes()
}

fn display_request_for_camera(
    camera: CameraFrame,
    viewport: RenderViewport,
    quality: CameraRenderQuality,
) -> GpuResidentDisplayRequest {
    display_request_for_camera_with_light(
        camera,
        viewport,
        quality,
        IsoLightState::default(),
        camera_state_axes(camera),
    )
}

fn display_request_for_camera_with_light(
    camera: CameraFrame,
    viewport: RenderViewport,
    quality: CameraRenderQuality,
    iso_light_state: IsoLightState,
    camera_axes: CameraAxes,
) -> GpuResidentDisplayRequest {
    GpuResidentDisplayRequest {
        camera,
        viewport,
        quality,
        iso_light_state,
        camera_axes,
    }
}

fn resident_u16_z_slab(
    layer_id: &str,
    active_z: u64,
) -> (DenseVolumeU16, ResidentBrickSetU16, Shape3D, Shape3D) {
    let layer_id = LayerId::new(layer_id).unwrap();
    let shape = Shape3D::new(4, 4, 4).unwrap();
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    for z in 0..shape.z() {
        for _y in 0..shape.y() {
            for _x in 0..shape.x() {
                values.push(if z == active_z { u16::MAX } else { 0 });
            }
        }
    }
    let volume = DenseVolumeU16::new(
        DatasetId::new("gpu-iso-depth-order").unwrap(),
        layer_id.clone(),
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap();
    let brick = VolumeBrickU16 {
        scale_level: 0,
        brick_index: SpatialBrickIndex::new(0, 0, 0),
        chunk_index: mirante4d_format::BrickIndex {
            t: 0,
            z: 0,
            y: 0,
            x: 0,
        },
        region: VolumeRegion::new(0, 0, 0, shape.z(), shape.y(), shape.x()).unwrap(),
        occupied: true,
        valid_voxel_count: shape.element_count().unwrap(),
        min: 0.0,
        max: f64::from(u16::MAX),
        volume: volume.clone(),
    };
    (
        volume,
        ResidentBrickSetU16::new(
            layer_id,
            TimeIndex::new(0),
            shape,
            GridToWorld::identity(),
            vec![brick],
        ),
        shape,
        Shape3D::new(1, 1, 1).unwrap(),
    )
}

fn resident_f32_z_slab(
    layer_id: &str,
    active_z: u64,
) -> (DenseVolumeF32, ResidentBrickSetF32, Shape3D, Shape3D) {
    let layer_id = LayerId::new(layer_id).unwrap();
    let shape = Shape3D::new(4, 4, 4).unwrap();
    let mut values = Vec::with_capacity(shape.element_count().unwrap() as usize);
    for z in 0..shape.z() {
        for _y in 0..shape.y() {
            for _x in 0..shape.x() {
                values.push(if z == active_z { 1.0 } else { 0.0 });
            }
        }
    }
    let volume = DenseVolumeF32::new(
        DatasetId::new("gpu-iso-depth-order-f32").unwrap(),
        layer_id.clone(),
        0,
        TimeIndex::new(0),
        shape,
        GridToWorld::identity(),
        values,
    )
    .unwrap();
    let brick = VolumeBrickF32 {
        scale_level: 0,
        brick_index: SpatialBrickIndex::new(0, 0, 0),
        chunk_index: mirante4d_format::BrickIndex {
            t: 0,
            z: 0,
            y: 0,
            x: 0,
        },
        region: VolumeRegion::new(0, 0, 0, shape.z(), shape.y(), shape.x()).unwrap(),
        occupied: true,
        valid_voxel_count: shape.element_count().unwrap(),
        min: 0.0,
        max: 1.0,
        volume: volume.clone(),
    };
    (
        volume,
        ResidentBrickSetF32::new(
            layer_id,
            TimeIndex::new(0),
            shape,
            GridToWorld::identity(),
            vec![brick],
        ),
        shape,
        Shape3D::new(1, 1, 1).unwrap(),
    )
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_resident_display_texture_matches_cpu_intensity_compositor() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();
    let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
    let brick = dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex::new(0),
            mirante4d_data::SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let resident = ResidentBrickSetU16::new(
        layer_id,
        TimeIndex::new(0),
        volume.shape,
        volume.grid_to_world,
        vec![brick],
    );
    let viewport = RenderViewport::new(16, 16).unwrap();
    let camera = front_orthographic_camera(&volume, 16.0);
    let red = display_transfer([1.0, 0.0, 0.0, 1.0], 0.6, f32::from(u16::MAX));
    let green = display_transfer([0.0, 1.0, 0.0, 1.0], 0.35, f32::from(u16::MAX));
    let renderer = GpuRenderer::new_blocking().unwrap();

    let (cpu_frame, cpu_diagnostics) =
        crate::render_camera_from_bricks(&resident, camera, viewport, CameraRenderMode::Mip)
            .unwrap();
    assert!(
        cpu_diagnostics.complete,
        "CPU resident-brick reference must be complete"
    );
    let expected = composite_intensity_channels(&[
        IntensityChannelFrame::new(&cpu_frame, red),
        IntensityChannelFrame::new(&cpu_frame, green),
    ])
    .unwrap();

    let display_frame = renderer
        .render_resident_channels_to_display_texture(
            &[
                GpuResidentDisplayChannel::U16 {
                    resident: &resident,
                    brick_shape,
                    brick_grid_shape,
                    mode: CameraRenderMode::Mip,
                    transfer: red,
                },
                GpuResidentDisplayChannel::U16 {
                    resident: &resident,
                    brick_shape,
                    brick_grid_shape,
                    mode: CameraRenderMode::Mip,
                    transfer: green,
                },
            ],
            display_request_for_camera(camera, viewport, CameraRenderQuality::voxel_exact()),
        )
        .unwrap();
    let actual = read_display_frame_rgba(&renderer, &display_frame).unwrap();

    assert_eq!(display_frame.viewport, viewport);
    assert_eq!(display_frame.diagnostics.channels, 2);
    assert_gpu_compute_timing_when_enabled(&renderer, &display_frame);
    assert_rgba_abs_diff_le(&actual, expected.pixels(), 1);

    let scene_color = SceneColorRgba::new(0, 0, 255, 255);
    let scene_layer = SceneLayer::new(
        SceneLayerId::new("gpu-display-scene-overlay").unwrap(),
        SceneLayerKind::Interaction,
    )
    .with_style(SceneStyle::new(scene_color))
    .with_object(SceneObject::new(
        SceneObjectId::new("screen-point").unwrap(),
        CoordinateSpace::Screen,
        SceneTime::Static,
        OcclusionPolicy::AlwaysOnTop,
        SceneGeometry::Point {
            position: DVec3::new(4.0, 4.0, 0.0),
            radius_px: 2.0,
        },
    ));
    let draw_list =
        extract_scene_draw_list(&[scene_layer], SceneFrameContext::new(TimeIndex::new(0)));
    let scene_frame = renderer
        .render_scene_layers_to_display_texture(display_frame, &draw_list, camera, viewport)
        .unwrap();
    let scene_actual = read_display_frame_rgba(&renderer, &scene_frame).unwrap();
    let scene_index = (4 * viewport.width + 4) as usize;
    let scene_pixel_offset = scene_index * 4;
    assert_eq!(
        &scene_actual[scene_pixel_offset..scene_pixel_offset + 4],
        &[0, 0, 255, 255]
    );
    assert_eq!(&scene_actual[0..4], &actual[0..4]);

    let stats_after_first = renderer.stats().unwrap();
    assert_eq!(stats_after_first.display_resource_cache_misses, 1);
    assert_eq!(stats_after_first.display_resource_cache_hits, 1);
    assert!(stats_after_first.display_resource_resident_bytes > 0);
    renderer
        .render_resident_channels_to_display_texture(
            &[
                GpuResidentDisplayChannel::U16 {
                    resident: &resident,
                    brick_shape,
                    brick_grid_shape,
                    mode: CameraRenderMode::Mip,
                    transfer: red,
                },
                GpuResidentDisplayChannel::U16 {
                    resident: &resident,
                    brick_shape,
                    brick_grid_shape,
                    mode: CameraRenderMode::Mip,
                    transfer: green,
                },
            ],
            display_request_for_camera(camera, viewport, CameraRenderQuality::voxel_exact()),
        )
        .unwrap();
    let stats_after_second = renderer.stats().unwrap();
    assert_eq!(stats_after_second.display_resource_cache_misses, 1);
    assert_eq!(stats_after_second.display_resource_cache_hits, 2);
    assert_eq!(
        stats_after_second.display_resource_resident_bytes,
        stats_after_first.display_resource_resident_bytes
    );
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_display_frame_blend_pass_matches_additive_display_frame_reference() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();
    let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
    let brick = dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex::new(0),
            mirante4d_data::SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let resident = ResidentBrickSetU16::new(
        layer_id,
        TimeIndex::new(0),
        volume.shape,
        volume.grid_to_world,
        vec![brick],
    );
    let viewport = RenderViewport::new(16, 16).unwrap();
    let camera = front_orthographic_camera(&volume, 16.0);
    let request = display_request_for_camera(camera, viewport, CameraRenderQuality::voxel_exact());
    let red = display_transfer([1.0, 0.0, 0.0, 1.0], 0.6, f32::from(u16::MAX));
    let green = display_transfer([0.0, 1.0, 0.0, 1.0], 0.35, f32::from(u16::MAX));
    let renderer = GpuRenderer::new_blocking().unwrap();

    let red_frame = renderer
        .render_resident_channels_to_display_texture(
            &[GpuResidentDisplayChannel::U16 {
                resident: &resident,
                brick_shape,
                brick_grid_shape,
                mode: CameraRenderMode::Mip,
                transfer: red,
            }],
            request,
        )
        .unwrap();
    let red_frame = renderer.detach_display_frame_texture(red_frame).unwrap();
    let red_actual = read_display_frame_rgba(&renderer, &red_frame).unwrap();
    let green_frame = renderer
        .render_resident_channels_to_display_texture(
            &[GpuResidentDisplayChannel::U16 {
                resident: &resident,
                brick_shape,
                brick_grid_shape,
                mode: CameraRenderMode::Mip,
                transfer: green,
            }],
            request,
        )
        .unwrap();
    let green_actual = read_display_frame_rgba(&renderer, &green_frame).unwrap();
    let expected = additive_display_frame_reference(&red_actual, &green_actual);

    let blended = renderer
        .blend_display_frames_to_texture(
            red_frame,
            &green_frame,
            GpuDisplayFrameBlendMode::Additive,
        )
        .unwrap();
    let actual = read_display_frame_rgba(&renderer, &blended).unwrap();

    assert_eq!(blended.viewport, viewport);
    assert_eq!(blended.diagnostics.channels, 2);
    assert_gpu_compute_timing_when_enabled(&renderer, &blended);
    assert_rgba_abs_diff_le(&actual, &expected, 1);
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_resident_display_texture_matches_cpu_same_ray_multi_channel_dvr() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_fixture(FixtureKind::BasicU16_16Cube, tempdir.path()).unwrap();
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset
        .read_u16_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
    let brick_shape = dataset.brick_shape(&layer_id).unwrap();
    let brick_grid_shape = dataset.brick_grid_shape(&layer_id).unwrap();
    let brick = dataset
        .read_u16_brick(
            &layer_id,
            TimeIndex::new(0),
            SpatialBrickIndex::new(0, 0, 0),
        )
        .unwrap();
    let resident = ResidentBrickSetU16::new(
        layer_id,
        TimeIndex::new(0),
        volume.shape,
        volume.grid_to_world,
        vec![brick],
    );
    let viewport = RenderViewport::new(16, 16).unwrap();
    let camera = front_orthographic_camera(&volume, 16.0);
    let red_transfer = display_transfer([1.0, 0.0, 0.0, 1.0], 0.75, f32::from(u16::MAX));
    let green_transfer = display_transfer([0.0, 1.0, 0.0, 1.0], 0.45, f32::from(u16::MAX));
    let density_scale = 1.25;
    let red_params = dvr_parameters_for_transfer(&red_transfer, density_scale);
    let green_params = dvr_parameters_for_transfer(&green_transfer, density_scale);
    let quality = CameraRenderQuality::smooth_linear();
    let renderer = GpuRenderer::new_blocking().unwrap();

    let (cpu_frame, cpu_diagnostics) = render_dvr_channels_from_bricks_with_quality(
        &[
            DvrResidentChannel::u16(&resident, red_params),
            DvrResidentChannel::u16(&resident, green_params),
        ],
        camera,
        viewport,
        quality,
    )
    .unwrap();
    assert!(
        cpu_diagnostics.complete,
        "CPU resident DVR reference must be complete"
    );
    let expected =
        composite_dvr_rgba_channels(&[DvrRgbaChannelFrame::new(cpu_frame.dvr_rgba().unwrap())])
            .unwrap();

    let display_frame = renderer
        .render_resident_channels_to_display_texture(
            &[
                GpuResidentDisplayChannel::U16 {
                    resident: &resident,
                    brick_shape,
                    brick_grid_shape,
                    mode: CameraRenderMode::Dvr {
                        parameters: red_params,
                    },
                    transfer: red_transfer,
                },
                GpuResidentDisplayChannel::U16 {
                    resident: &resident,
                    brick_shape,
                    brick_grid_shape,
                    mode: CameraRenderMode::Dvr {
                        parameters: green_params,
                    },
                    transfer: green_transfer,
                },
            ],
            display_request_for_camera(camera, viewport, quality),
        )
        .unwrap();
    let actual = read_display_frame_rgba(&renderer, &display_frame).unwrap();

    assert_eq!(display_frame.viewport, viewport);
    assert_eq!(display_frame.diagnostics.channels, 2);
    assert_eq!(display_frame.diagnostics.output_bytes, 0);
    assert_gpu_compute_timing_when_enabled(&renderer, &display_frame);
    assert_rgba_abs_diff_le(&actual, expected.pixels(), 2);
    let stats_after_first = renderer.stats().unwrap();

    let reversed_frame = renderer
        .render_resident_channels_to_display_texture(
            &[
                GpuResidentDisplayChannel::U16 {
                    resident: &resident,
                    brick_shape,
                    brick_grid_shape,
                    mode: CameraRenderMode::Dvr {
                        parameters: green_params,
                    },
                    transfer: green_transfer,
                },
                GpuResidentDisplayChannel::U16 {
                    resident: &resident,
                    brick_shape,
                    brick_grid_shape,
                    mode: CameraRenderMode::Dvr {
                        parameters: red_params,
                    },
                    transfer: red_transfer,
                },
            ],
            display_request_for_camera(camera, viewport, quality),
        )
        .unwrap();
    let reversed = read_display_frame_rgba(&renderer, &reversed_frame).unwrap();
    assert_rgba_abs_diff_le(&reversed, &actual, 1);
    let stats_after_second = renderer.stats().unwrap();
    assert_eq!(
        stats_after_second.display_resource_resident_bytes,
        stats_after_first.display_resource_resident_bytes
    );
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_resident_display_texture_matches_cpu_same_ray_multi_channel_f32_dvr() {
    let (camera_volume, near_resident, brick_shape, brick_grid_shape) =
        resident_f32_z_slab("near-f32-dvr", 0);
    let (_, far_resident, far_brick_shape, far_brick_grid_shape) =
        resident_f32_z_slab("far-f32-dvr", 3);
    let viewport = RenderViewport::new(4, 4).unwrap();
    let camera = front_orthographic_camera_f32(&camera_volume, 4.0);
    let red_transfer = display_transfer_window([1.0, 0.0, 0.0, 1.0], 0.7, 0.0, 1.0);
    let green_transfer = display_transfer_window([0.0, 1.0, 0.0, 1.0], 0.55, 0.0, 1.0);
    let density_scale = 1.4;
    let red_params = dvr_parameters_for_transfer(&red_transfer, density_scale);
    let green_params = dvr_parameters_for_transfer(&green_transfer, density_scale);
    let quality = CameraRenderQuality::smooth_linear();
    let renderer = GpuRenderer::new_blocking().unwrap();

    let (cpu_frame, cpu_diagnostics) = render_dvr_channels_from_bricks_with_quality(
        &[
            DvrResidentChannel::f32(&near_resident, green_params),
            DvrResidentChannel::f32(&far_resident, red_params),
        ],
        camera,
        viewport,
        quality,
    )
    .unwrap();
    assert!(
        cpu_diagnostics.complete,
        "CPU resident f32 DVR reference must be complete"
    );
    let expected =
        composite_dvr_rgba_channels(&[DvrRgbaChannelFrame::new(cpu_frame.dvr_rgba().unwrap())])
            .unwrap();

    let display_frame = renderer
        .render_resident_channels_to_display_texture(
            &[
                GpuResidentDisplayChannel::F32 {
                    resident: &near_resident,
                    brick_shape,
                    brick_grid_shape,
                    mode: CameraRenderModeF32::Dvr {
                        parameters: green_params,
                    },
                    transfer: green_transfer,
                },
                GpuResidentDisplayChannel::F32 {
                    resident: &far_resident,
                    brick_shape: far_brick_shape,
                    brick_grid_shape: far_brick_grid_shape,
                    mode: CameraRenderModeF32::Dvr {
                        parameters: red_params,
                    },
                    transfer: red_transfer,
                },
            ],
            display_request_for_camera(camera, viewport, quality),
        )
        .unwrap();
    let actual = read_display_frame_rgba(&renderer, &display_frame).unwrap();

    assert_eq!(display_frame.viewport, viewport);
    assert_eq!(display_frame.diagnostics.channels, 2);
    assert_eq!(display_frame.diagnostics.output_bytes, 0);
    assert_gpu_compute_timing_when_enabled(&renderer, &display_frame);
    assert_rgba_abs_diff_le(&actual, expected.pixels(), 3);
    let stats_after_first = renderer.stats().unwrap();

    let reversed_frame = renderer
        .render_resident_channels_to_display_texture(
            &[
                GpuResidentDisplayChannel::F32 {
                    resident: &far_resident,
                    brick_shape: far_brick_shape,
                    brick_grid_shape: far_brick_grid_shape,
                    mode: CameraRenderModeF32::Dvr {
                        parameters: red_params,
                    },
                    transfer: red_transfer,
                },
                GpuResidentDisplayChannel::F32 {
                    resident: &near_resident,
                    brick_shape,
                    brick_grid_shape,
                    mode: CameraRenderModeF32::Dvr {
                        parameters: green_params,
                    },
                    transfer: green_transfer,
                },
            ],
            display_request_for_camera(camera, viewport, quality),
        )
        .unwrap();
    let reversed = read_display_frame_rgba(&renderer, &reversed_frame).unwrap();
    assert_rgba_abs_diff_le(&reversed, expected.pixels(), 3);
    assert_rgba_abs_diff_le(&reversed, &actual, 1);
    let stats_after_second = renderer.stats().unwrap();
    assert_eq!(
        stats_after_second.display_resource_resident_bytes,
        stats_after_first.display_resource_resident_bytes
    );
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_resident_display_texture_matches_cpu_depth_sorted_multi_channel_iso() {
    let (camera_volume, near_resident, brick_shape, brick_grid_shape) =
        resident_u16_z_slab("near", 0);
    let (_, far_resident, far_brick_shape, far_brick_grid_shape) = resident_u16_z_slab("far", 3);
    let viewport = RenderViewport::new(4, 4).unwrap();
    let camera = front_orthographic_camera(&camera_volume, 4.0);
    let mode = CameraRenderMode::Isosurface {
        parameters: iso_u16_threshold(u16::MAX / 2),
    };
    let near_green = display_transfer([0.0, 1.0, 0.0, 1.0], 1.0, f32::from(u16::MAX));
    let far_red = display_transfer([1.0, 0.0, 0.0, 1.0], 1.0, f32::from(u16::MAX));
    let light_state = IsoLightState::default();
    let camera_axes = camera_state_axes(camera);
    let renderer = GpuRenderer::new_blocking().unwrap();

    let (near_frame, near_diagnostics) =
        crate::render_camera_from_bricks(&near_resident, camera, viewport, mode).unwrap();
    let (far_frame, far_diagnostics) =
        crate::render_camera_from_bricks(&far_resident, camera, viewport, mode).unwrap();
    assert!(
        near_diagnostics.complete,
        "near CPU resident ISO reference must be complete"
    );
    assert!(
        far_diagnostics.complete,
        "far CPU resident ISO reference must be complete"
    );
    let near_surface = near_frame.iso_surface().unwrap();
    let far_surface = far_frame.iso_surface().unwrap();
    let center_index = (2 * viewport.width + 2) as usize;
    assert!(near_surface.is_covered_index(center_index));
    assert!(far_surface.is_covered_index(center_index));
    assert!(
        far_surface.hit_depth()[center_index] > near_surface.hit_depth()[center_index],
        "fixture must put the red surface behind the green surface"
    );

    let expected = composite_iso_surface_channels(
        &[
            IsoSurfaceChannelFrame::new(near_surface, near_green),
            IsoSurfaceChannelFrame::new(far_surface, far_red),
        ],
        light_state,
        camera_axes,
    )
    .unwrap();
    let expected_center = expected.pixel_rgba(2, 2).unwrap();
    assert!(
        expected_center[1] > expected_center[0],
        "fixture must make the depth-near green surface dominate the center pixel"
    );

    let display_frame = renderer
        .render_resident_channels_to_display_texture(
            &[
                GpuResidentDisplayChannel::U16 {
                    resident: &near_resident,
                    brick_shape,
                    brick_grid_shape,
                    mode,
                    transfer: near_green,
                },
                GpuResidentDisplayChannel::U16 {
                    resident: &far_resident,
                    brick_shape: far_brick_shape,
                    brick_grid_shape: far_brick_grid_shape,
                    mode,
                    transfer: far_red,
                },
            ],
            display_request_for_camera_with_light(
                camera,
                viewport,
                CameraRenderQuality::voxel_exact(),
                light_state,
                camera_axes,
            ),
        )
        .unwrap();
    let actual = read_display_frame_rgba(&renderer, &display_frame).unwrap();

    assert_eq!(display_frame.viewport, viewport);
    assert_eq!(display_frame.diagnostics.channels, 2);
    assert_gpu_compute_timing_when_enabled(&renderer, &display_frame);
    assert_rgba_abs_diff_le(&actual, expected.pixels(), 1);
    let stats_after_first = renderer.stats().unwrap();

    let reversed_frame = renderer
        .render_resident_channels_to_display_texture(
            &[
                GpuResidentDisplayChannel::U16 {
                    resident: &far_resident,
                    brick_shape: far_brick_shape,
                    brick_grid_shape: far_brick_grid_shape,
                    mode,
                    transfer: far_red,
                },
                GpuResidentDisplayChannel::U16 {
                    resident: &near_resident,
                    brick_shape,
                    brick_grid_shape,
                    mode,
                    transfer: near_green,
                },
            ],
            display_request_for_camera_with_light(
                camera,
                viewport,
                CameraRenderQuality::voxel_exact(),
                light_state,
                camera_axes,
            ),
        )
        .unwrap();
    let reversed = read_display_frame_rgba(&renderer, &reversed_frame).unwrap();
    assert_rgba_abs_diff_le(&reversed, expected.pixels(), 1);
    assert_rgba_abs_diff_le(&reversed, &actual, 1);
    let stats_after_second = renderer.stats().unwrap();
    assert_eq!(
        stats_after_second.display_resource_resident_bytes,
        stats_after_first.display_resource_resident_bytes
    );
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_resident_display_texture_matches_cpu_depth_sorted_multi_channel_f32_iso() {
    let (camera_volume, near_resident, brick_shape, brick_grid_shape) =
        resident_f32_z_slab("near-f32", 0);
    let (_, far_resident, far_brick_shape, far_brick_grid_shape) =
        resident_f32_z_slab("far-f32", 3);
    let viewport = RenderViewport::new(4, 4).unwrap();
    let camera = front_orthographic_camera_f32(&camera_volume, 4.0);
    let mode = CameraRenderModeF32::Isosurface {
        parameters: iso_f32_threshold(0.5, 0.0, 1.0),
    };
    let near_green = display_transfer_window([0.0, 1.0, 0.0, 1.0], 1.0, 0.0, 1.0);
    let far_red = display_transfer_window([1.0, 0.0, 0.0, 1.0], 1.0, 0.0, 1.0);
    let light_state = IsoLightState::default();
    let camera_axes = camera_state_axes(camera);
    let renderer = GpuRenderer::new_blocking().unwrap();

    let (near_frame, near_diagnostics) =
        render_camera_f32_from_bricks(&near_resident, camera, viewport, mode).unwrap();
    let (far_frame, far_diagnostics) =
        render_camera_f32_from_bricks(&far_resident, camera, viewport, mode).unwrap();
    assert!(
        near_diagnostics.complete,
        "near CPU resident f32 ISO reference must be complete"
    );
    assert!(
        far_diagnostics.complete,
        "far CPU resident f32 ISO reference must be complete"
    );
    let near_surface = near_frame.iso_surface().unwrap();
    let far_surface = far_frame.iso_surface().unwrap();
    let center_index = (2 * viewport.width + 2) as usize;
    assert!(near_surface.is_covered_index(center_index));
    assert!(far_surface.is_covered_index(center_index));
    assert!(
        far_surface.hit_depth()[center_index] > near_surface.hit_depth()[center_index],
        "fixture must put the red f32 surface behind the green f32 surface"
    );

    let expected = composite_iso_surface_f32_channels(
        &[
            IsoSurfaceChannelFrameF32::new(near_surface, near_green),
            IsoSurfaceChannelFrameF32::new(far_surface, far_red),
        ],
        light_state,
        camera_axes,
    )
    .unwrap();
    let expected_center = expected.pixel_rgba(2, 2).unwrap();
    assert!(
        expected_center[1] > expected_center[0],
        "fixture must make the depth-near green f32 surface dominate the center pixel"
    );

    let display_frame = renderer
        .render_resident_channels_to_display_texture(
            &[
                GpuResidentDisplayChannel::F32 {
                    resident: &near_resident,
                    brick_shape,
                    brick_grid_shape,
                    mode,
                    transfer: near_green,
                },
                GpuResidentDisplayChannel::F32 {
                    resident: &far_resident,
                    brick_shape: far_brick_shape,
                    brick_grid_shape: far_brick_grid_shape,
                    mode,
                    transfer: far_red,
                },
            ],
            display_request_for_camera_with_light(
                camera,
                viewport,
                CameraRenderQuality::voxel_exact(),
                light_state,
                camera_axes,
            ),
        )
        .unwrap();
    let actual = read_display_frame_rgba(&renderer, &display_frame).unwrap();

    assert_eq!(display_frame.viewport, viewport);
    assert_eq!(display_frame.diagnostics.channels, 2);
    assert_gpu_compute_timing_when_enabled(&renderer, &display_frame);
    assert_rgba_abs_diff_le(&actual, expected.pixels(), 1);
    let stats_after_first = renderer.stats().unwrap();

    let reversed_frame = renderer
        .render_resident_channels_to_display_texture(
            &[
                GpuResidentDisplayChannel::F32 {
                    resident: &far_resident,
                    brick_shape: far_brick_shape,
                    brick_grid_shape: far_brick_grid_shape,
                    mode,
                    transfer: far_red,
                },
                GpuResidentDisplayChannel::F32 {
                    resident: &near_resident,
                    brick_shape,
                    brick_grid_shape,
                    mode,
                    transfer: near_green,
                },
            ],
            display_request_for_camera_with_light(
                camera,
                viewport,
                CameraRenderQuality::voxel_exact(),
                light_state,
                camera_axes,
            ),
        )
        .unwrap();
    let reversed = read_display_frame_rgba(&renderer, &reversed_frame).unwrap();
    assert_rgba_abs_diff_le(&reversed, expected.pixels(), 1);
    assert_rgba_abs_diff_le(&reversed, &actual, 1);
    let stats_after_second = renderer.stats().unwrap();
    assert_eq!(
        stats_after_second.display_resource_resident_bytes,
        stats_after_first.display_resource_resident_bytes
    );
}

#[test]
#[ignore = "requires a usable non-CPU GPU adapter"]
fn gpu_resident_display_texture_matches_cpu_f32_display_compositor() {
    let tempdir = tempfile::tempdir().unwrap();
    let root = write_three_brick_f32_gpu_fixture(tempdir.path());
    let dataset = DatasetHandle::open(&root).unwrap();
    let layer_id = dataset.first_layer_id().unwrap();
    let volume = dataset
        .read_f32_volume(&layer_id, TimeIndex::new(0))
        .unwrap();
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
            .read_f32_brick(&layer_id, TimeIndex::new(0), index)
            .unwrap()
    })
    .collect::<Vec<_>>();
    let resident = ResidentBrickSetF32::new(
        layer_id,
        TimeIndex::new(0),
        volume.shape,
        volume.grid_to_world,
        bricks,
    );
    let viewport = RenderViewport::new(6, 2).unwrap();
    let camera = front_orthographic_camera_f32(&volume, 2.0);
    let transfer = display_transfer_window([0.25, 0.75, 1.0, 1.0], 0.65, -6.0, 6.0);
    let renderer = GpuRenderer::new_blocking().unwrap();

    let (cpu_mip, cpu_mip_diagnostics) =
        render_camera_f32_from_bricks(&resident, camera, viewport, CameraRenderModeF32::Mip)
            .unwrap();
    assert!(
        cpu_mip_diagnostics.complete,
        "CPU resident f32 MIP reference must be complete"
    );
    let expected_mip =
        composite_f32_intensity_channels(&[IntensityChannelFrameF32::new(&cpu_mip, transfer)])
            .unwrap();
    let mip_frame = renderer
        .render_resident_channels_to_display_texture(
            &[GpuResidentDisplayChannel::F32 {
                resident: &resident,
                brick_shape,
                brick_grid_shape,
                mode: CameraRenderModeF32::Mip,
                transfer,
            }],
            display_request_for_camera(camera, viewport, CameraRenderQuality::voxel_exact()),
        )
        .unwrap();
    let actual_mip = read_display_frame_rgba(&renderer, &mip_frame).unwrap();
    assert_gpu_compute_timing_when_enabled(&renderer, &mip_frame);
    assert_rgba_abs_diff_le(&actual_mip, expected_mip.pixels(), 1);

    let dvr_mode = CameraRenderModeF32::Dvr {
        parameters: dvr_parameters(-6.0, 6.0, 8.0, false),
    };
    let (cpu_dvr, cpu_dvr_diagnostics) =
        render_camera_f32_from_bricks(&resident, camera, viewport, dvr_mode).unwrap();
    assert!(
        cpu_dvr_diagnostics.complete,
        "CPU resident f32 DVR reference must be complete"
    );
    let expected_dvr =
        composite_dvr_rgba_channels(&[DvrRgbaChannelFrame::new(cpu_dvr.dvr_rgba().unwrap())])
            .unwrap();
    let dvr_frame = renderer
        .render_resident_channels_to_display_texture(
            &[GpuResidentDisplayChannel::F32 {
                resident: &resident,
                brick_shape,
                brick_grid_shape,
                mode: dvr_mode,
                transfer,
            }],
            display_request_for_camera(camera, viewport, CameraRenderQuality::voxel_exact()),
        )
        .unwrap();
    let actual_dvr = read_display_frame_rgba(&renderer, &dvr_frame).unwrap();
    assert_gpu_compute_timing_when_enabled(&renderer, &dvr_frame);
    assert_rgba_abs_diff_le(&actual_dvr, expected_dvr.pixels(), 1);
}

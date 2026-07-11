use std::borrow::Cow;

use mirante4d_core::CameraState;
use mirante4d_data::DenseVolumeU16;
use wgpu::util::DeviceExt;

use super::{
    GpuMipOutput, GpuRenderError, GpuRenderTimings, GpuRenderer, WORKGROUP_SIZE_X,
    WORKGROUP_SIZE_Y,
    adapter::request_device,
    buffers::checked_u32,
    buffers::storage_entry,
    decode::gpu_output_covered,
    decode::gpu_output_value_u16,
    readback::{
        submit_and_read_u32_with_optional_timestamp_from_device, timestamp_query_pair_for_device,
    },
    shaders::MIP_SHADER,
    volume_cache::render_upload_samples_u16,
};
use crate::{
    CameraRenderMode, MipImageU16, PixelCoverage, RenderError, RenderViewport, frame_diagnostics,
};

pub fn render_mip_z_wgpu_blocking(volume: &DenseVolumeU16) -> Result<GpuMipOutput, GpuRenderError> {
    pollster::block_on(render_mip_z_wgpu(volume))
}

pub fn render_camera_mip_wgpu_blocking(
    volume: &DenseVolumeU16,
    camera: CameraState,
    viewport: RenderViewport,
) -> Result<GpuMipOutput, GpuRenderError> {
    let renderer = GpuRenderer::new_blocking()?;
    renderer.render_camera_mip(volume, camera, viewport)
}

pub fn render_camera_wgpu_blocking(
    volume: &DenseVolumeU16,
    camera: CameraState,
    viewport: RenderViewport,
    mode: CameraRenderMode,
) -> Result<GpuMipOutput, GpuRenderError> {
    let renderer = GpuRenderer::new_blocking()?;
    renderer.render_camera(volume, camera, viewport, mode)
}

pub async fn render_mip_z_wgpu(volume: &DenseVolumeU16) -> Result<GpuMipOutput, GpuRenderError> {
    if volume.values().is_empty() {
        return Err(RenderError::EmptyVolume.into());
    }

    let width = checked_u32("x", volume.shape.x)?;
    let height = checked_u32("y", volume.shape.y)?;
    let depth = checked_u32("z", volume.shape.z)?;
    let input_values = render_upload_samples_u16(volume).into_owned();

    let (device, queue, adapter_diagnostics) =
        request_device("mirante4d-render-test-device").await?;

    let input_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mirante4d-mip-input-u32"),
        contents: bytemuck::cast_slice(&input_values),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let output_len = (u64::from(width) * u64::from(height)) as usize;
    let output_bytes = (output_len * std::mem::size_of::<u32>()) as wgpu::BufferAddress;
    let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-mip-output-u32"),
        size: output_bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mirante4d-mip-readback-u32"),
        size: output_bytes,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let params = [width, height, depth, 0u32];
    let params_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mirante4d-mip-params"),
        contents: bytemuck::cast_slice(&params),
        usage: wgpu::BufferUsages::UNIFORM,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("mirante4d-mip-wgsl"),
        source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(MIP_SHADER)),
    });
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mirante4d-mip-bind-group-layout"),
        entries: &[
            storage_entry(0, true),
            storage_entry(1, false),
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("mirante4d-mip-pipeline-layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("mirante4d-mip-compute-pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: Some("mip_main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mirante4d-mip-bind-group"),
        layout: &bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: input_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("mirante4d-mip-command-encoder"),
    });
    let timestamp = timestamp_query_pair_for_device(
        &device,
        adapter_diagnostics.timestamp_queries_enabled,
        "mirante4d-standalone-mip-timestamp",
    );
    let timestamp_writes = timestamp
        .as_ref()
        .map(super::readback::GpuTimestampQueryPair::compute_pass_writes);
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("mirante4d-mip-compute-pass"),
            timestamp_writes,
        });
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            width.div_ceil(WORKGROUP_SIZE_X),
            height.div_ceil(WORKGROUP_SIZE_Y),
            1,
        );
    }
    encoder.copy_buffer_to_buffer(&output_buffer, 0, &readback_buffer, 0, output_bytes);
    let (output_u32, gpu_compute_ns) = submit_and_read_u32_with_optional_timestamp_from_device(
        &device,
        &queue,
        encoder,
        readback_buffer,
        timestamp,
    )?;

    let pixels: Vec<u16> = output_u32
        .iter()
        .copied()
        .map(gpu_output_value_u16)
        .collect();
    let coverage = output_u32
        .iter()
        .copied()
        .map(|value| u8::from(gpu_output_covered(value)))
        .collect();
    let frame = frame_diagnostics(volume.render_valid_voxel_count(), &pixels);
    Ok(GpuMipOutput {
        image: MipImageU16::try_new(
            u64::from(width),
            u64::from(height),
            pixels,
            PixelCoverage::Mask(coverage),
        )?,
        frame,
        brick_frame: None,
        timings: gpu_compute_ns.map(|gpu_compute_ns| GpuRenderTimings {
            upload_ns: 0,
            gpu_compute_ns: Some(gpu_compute_ns),
        }),
        adapter: adapter_diagnostics,
    })
}

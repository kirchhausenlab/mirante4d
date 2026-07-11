use mirante4d_data::DenseVolumeU16;
use mirante4d_render_api::CameraFrame;
use wgpu::util::DeviceExt;

use super::{
    GpuMipOutput, GpuRenderError, GpuRenderTimings, GpuRenderer, WORKGROUP_SIZE_X,
    WORKGROUP_SIZE_Y,
    buffers::{
        checked_buffer_byte_count, checked_u32, validate_general_buffer_bytes,
        validate_storage_buffer_bytes,
    },
    decode::{
        GPU_SURFACE_OUTPUT_U16_FIELDS, decode_gpu_dvr_rgba_u16, decode_gpu_iso_surface_u16,
        gpu_output_covered, gpu_output_value_u16, mode_uses_dvr_u16, mode_uses_iso_u16,
    },
    params::{
        GPU_PARAM_DVR_ALPHA_MULTIPLIER_INDEX, GPU_PARAM_DVR_COLOR_B_INDEX,
        GPU_PARAM_DVR_COLOR_G_INDEX, GPU_PARAM_DVR_COLOR_R_INDEX,
        GPU_PARAM_DVR_OPACITY_GAMMA_INDEX, GPU_PARAM_DVR_OPACITY_HIGH_INDEX,
        GPU_PARAM_DVR_OPACITY_LOW_INDEX, GPU_PARAM_ISO_DISPLAY_HIGH_INDEX,
        GPU_PARAM_ISO_DISPLAY_LOW_INDEX, GPU_PARAM_ISO_GAMMA_INDEX, GPU_PARAM_ISO_LEVEL_INDEX,
        apply_gpu_quality_params, camera_grid_params, gpu_mode_params, projection_code,
    },
};
use crate::{
    CameraRenderMode, CameraRenderQuality, MipImageU16, PixelCoverage, RenderError, RenderViewport,
    frame_diagnostics,
};

impl GpuRenderer {
    pub fn render_camera_mip(
        &self,
        volume: &DenseVolumeU16,
        camera: CameraFrame,
        viewport: RenderViewport,
    ) -> Result<GpuMipOutput, GpuRenderError> {
        self.render_camera(volume, camera, viewport, CameraRenderMode::Mip)
    }

    pub fn render_camera(
        &self,
        volume: &DenseVolumeU16,
        camera: CameraFrame,
        viewport: RenderViewport,
        mode: CameraRenderMode,
    ) -> Result<GpuMipOutput, GpuRenderError> {
        self.render_camera_with_quality(
            volume,
            camera,
            viewport,
            mode,
            CameraRenderQuality::voxel_exact(),
        )
    }

    pub fn render_camera_with_quality(
        &self,
        volume: &DenseVolumeU16,
        camera: CameraFrame,
        viewport: RenderViewport,
        mode: CameraRenderMode,
        quality: CameraRenderQuality,
    ) -> Result<GpuMipOutput, GpuRenderError> {
        if volume.values().is_empty() {
            return Err(RenderError::EmptyVolume.into());
        }

        let viewport_width = checked_u32("viewport_width", viewport.width)?;
        let viewport_height = checked_u32("viewport_height", viewport.height)?;
        let shape_x = checked_u32("x", volume.shape.x())?;
        let shape_y = checked_u32("y", volume.shape.y())?;
        let shape_z = checked_u32("z", volume.shape.z())?;
        let mut camera_params = camera_grid_params(volume, camera, viewport)?;
        let mode_params = gpu_mode_params(volume, mode)?;
        camera_params[15] = mode_params.density_scale;
        camera_params[GPU_PARAM_ISO_LEVEL_INDEX] = mode_params.iso_display_level;
        camera_params[GPU_PARAM_ISO_DISPLAY_LOW_INDEX] = mode_params.iso_transfer.window.low();
        camera_params[GPU_PARAM_ISO_DISPLAY_HIGH_INDEX] = mode_params.iso_transfer.window.high();
        camera_params[GPU_PARAM_ISO_GAMMA_INDEX] = mode_params.iso_transfer.curve.gamma_value();
        camera_params[GPU_PARAM_DVR_COLOR_R_INDEX] = mode_params.dvr_color_rgb[0];
        camera_params[GPU_PARAM_DVR_COLOR_G_INDEX] = mode_params.dvr_color_rgb[1];
        camera_params[GPU_PARAM_DVR_COLOR_B_INDEX] = mode_params.dvr_color_rgb[2];
        camera_params[GPU_PARAM_DVR_ALPHA_MULTIPLIER_INDEX] = mode_params.dvr_alpha_multiplier;
        camera_params[GPU_PARAM_DVR_OPACITY_LOW_INDEX] =
            mode_params.dvr_opacity_transfer.window.low();
        camera_params[GPU_PARAM_DVR_OPACITY_HIGH_INDEX] =
            mode_params.dvr_opacity_transfer.window.high();
        camera_params[GPU_PARAM_DVR_OPACITY_GAMMA_INDEX] =
            mode_params.dvr_opacity_transfer.curve.gamma_value();
        apply_gpu_quality_params(&mut camera_params, quality);
        let input_buffer = self.cached_volume_buffer(volume)?;
        let output_len = (u64::from(viewport_width) * u64::from(viewport_height)) as usize;
        let output_bytes = checked_buffer_byte_count(
            "camera uint16 output",
            output_len,
            GPU_SURFACE_OUTPUT_U16_FIELDS * std::mem::size_of::<u32>(),
        )?;
        validate_storage_buffer_bytes(&self.device.limits(), "camera uint16 output", output_bytes)?;
        validate_general_buffer_bytes(
            &self.device.limits(),
            "camera uint16 readback",
            output_bytes,
        )?;
        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-camera-mip-output-u32"),
            size: output_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-camera-mip-readback-u32"),
            size: output_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let params_u32 = [
            viewport_width,
            viewport_height,
            shape_x,
            shape_y,
            shape_z,
            projection_code(crate::current_camera::projection(camera)),
            mode_params.mode_code,
            mode_params.iso_invert,
        ];
        let params_u32_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-camera-mip-params-u32"),
                contents: bytemuck::cast_slice(&params_u32),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let params_f32_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-camera-mip-params-f32"),
                contents: bytemuck::cast_slice(&camera_params),
                usage: wgpu::BufferUsages::STORAGE,
            });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-camera-mip-bind-group"),
            layout: &self.camera_mip_bind_group_layout,
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
                    resource: params_u32_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: params_f32_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-camera-mip-command-encoder"),
            });
        let timestamp = self.timestamp_query_pair("mirante4d-camera-mip-timestamp");
        let timestamp_writes = timestamp
            .as_ref()
            .map(super::readback::GpuTimestampQueryPair::compute_pass_writes);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-camera-mip-compute-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.camera_mip_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                viewport_width.div_ceil(WORKGROUP_SIZE_X),
                viewport_height.div_ceil(WORKGROUP_SIZE_Y),
                1,
            );
        }
        encoder.copy_buffer_to_buffer(
            &output_buffer,
            0,
            &readback_buffer,
            0,
            output_bytes as wgpu::BufferAddress,
        );

        let (output_u32, gpu_compute_ns) =
            self.submit_and_read_u32_with_optional_timestamp(encoder, readback_buffer, timestamp)?;
        let pixels: Vec<u16> = output_u32
            .chunks_exact(GPU_SURFACE_OUTPUT_U16_FIELDS)
            .map(|record| gpu_output_value_u16(record[0]))
            .collect();
        let coverage = output_u32
            .chunks_exact(GPU_SURFACE_OUTPUT_U16_FIELDS)
            .map(|record| u8::from(gpu_output_covered(record[0])))
            .collect();
        let iso_surface = decode_gpu_iso_surface_u16(
            viewport.width,
            viewport.height,
            &output_u32,
            mode_uses_iso_u16(mode),
        )?;
        let dvr_rgba = decode_gpu_dvr_rgba_u16(
            viewport.width,
            viewport.height,
            &output_u32,
            mode_uses_dvr_u16(mode),
        )?;
        let frame = frame_diagnostics(volume.render_valid_voxel_count(), &pixels);
        Ok(GpuMipOutput {
            image: MipImageU16::try_new_with_mode_frames(
                viewport.width,
                viewport.height,
                pixels,
                PixelCoverage::Mask(coverage),
                iso_surface,
                dvr_rgba,
            )?,
            frame,
            brick_frame: None,
            timings: gpu_compute_ns.map(|gpu_compute_ns| GpuRenderTimings {
                upload_ns: 0,
                gpu_compute_ns: Some(gpu_compute_ns),
            }),
            adapter: self.adapter.clone(),
        })
    }
}

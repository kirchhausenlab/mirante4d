use std::time::Instant;

use mirante4d_domain::Shape3D;
use mirante4d_render_api::CameraFrame;
use wgpu::util::DeviceExt;

use super::{
    GpuMipOutputF32, GpuRenderError, GpuRenderTimings, GpuRenderer, WORKGROUP_SIZE_X,
    WORKGROUP_SIZE_Y,
    buffers::{
        checked_buffer_byte_count, checked_u32, validate_general_buffer_bytes,
        validate_storage_buffer_bytes,
    },
    decode::{
        GPU_SURFACE_OUTPUT_F32_FIELDS, decode_gpu_dvr_rgba_f32, decode_gpu_iso_surface_f32,
        gpu_output_f32_covered, gpu_output_f32_missing_samples, mode_uses_dvr_f32,
        mode_uses_iso_f32,
    },
    display::{GpuF32CameraOutputBuffer, f32_camera_output_bytes},
    display_resources::GpuF32CameraDisplayBufferSpec,
    duration_ns_u64,
    params::{
        GPU_PARAM_DVR_ALPHA_MULTIPLIER_INDEX, GPU_PARAM_DVR_COLOR_B_INDEX,
        GPU_PARAM_DVR_COLOR_G_INDEX, GPU_PARAM_DVR_COLOR_R_INDEX,
        GPU_PARAM_DVR_OPACITY_GAMMA_INDEX, GPU_PARAM_DVR_OPACITY_HIGH_INDEX,
        GPU_PARAM_DVR_OPACITY_LOW_INDEX, GPU_PARAM_ISO_DISPLAY_HIGH_INDEX,
        GPU_PARAM_ISO_DISPLAY_LOW_INDEX, GPU_PARAM_ISO_GAMMA_INDEX, GPU_PARAM_ISO_LEVEL_INDEX,
        apply_gpu_quality_params, camera_grid_params_f32_for_transform,
        gpu_mode_params_f32_for_transform, projection_code,
    },
};
use crate::{
    BrickFrameDiagnosticsF32, CameraRenderModeF32, CameraRenderQuality, DvrRgbaFrame,
    FrameDiagnosticsF32, IsoSurfaceFrameF32, MipImageF32, PixelCoverage, RenderError,
    RenderViewport, ResidentBrickSetF32, frame_diagnostics_f32,
};

#[derive(Debug, Clone, PartialEq)]
struct GpuBrickedRawOutputF32 {
    raw_words: Vec<u32>,
    pixels: Vec<f32>,
    coverage: Vec<u8>,
    iso_surface: Option<IsoSurfaceFrameF32>,
    dvr_rgba: Option<DvrRgbaFrame>,
    frame: FrameDiagnosticsF32,
    brick_frame: BrickFrameDiagnosticsF32,
    timings: GpuRenderTimings,
}

impl GpuRenderer {
    #[allow(clippy::too_many_arguments)]
    fn render_camera_f32_from_bricks_raw_pairs(
        &self,
        resident: &ResidentBrickSetF32,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: CameraFrame,
        viewport: RenderViewport,
        mode: CameraRenderModeF32,
        quality: CameraRenderQuality,
    ) -> Result<GpuBrickedRawOutputF32, GpuRenderError> {
        if resident.bricks().is_empty() {
            return Err(RenderError::InvalidBrickAtlas("resident brick set is empty").into());
        }

        let viewport_width = checked_u32("viewport_width", viewport.width)?;
        let viewport_height = checked_u32("viewport_height", viewport.height)?;
        let shape_x = checked_u32("x", resident.volume_shape.x())?;
        let shape_y = checked_u32("y", resident.volume_shape.y())?;
        let shape_z = checked_u32("z", resident.volume_shape.z())?;
        let upload_started = Instant::now();
        let atlas = self.cached_brick_atlas_f32(resident, brick_shape, brick_grid_shape)?;
        let mut timings = GpuRenderTimings {
            upload_ns: duration_ns_u64(upload_started.elapsed()),
            ..Default::default()
        };
        let brick_x = checked_u32("brick_x", atlas.brick_shape.x())?;
        let brick_y = checked_u32("brick_y", atlas.brick_shape.y())?;
        let brick_z = checked_u32("brick_z", atlas.brick_shape.z())?;
        let grid_x = checked_u32("grid_x", atlas.brick_grid_shape.x())?;
        let grid_y = checked_u32("grid_y", atlas.brick_grid_shape.y())?;
        let grid_z = checked_u32("grid_z", atlas.brick_grid_shape.z())?;
        let brick_voxel_count = checked_u32("brick_voxel_count", atlas.brick_voxel_count)?;

        let mut camera_params =
            camera_grid_params_f32_for_transform(resident.grid_to_world, camera, viewport)?;
        let mode_params = gpu_mode_params_f32_for_transform(resident.grid_to_world, mode)?;
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
        let output_len = (u64::from(viewport_width) * u64::from(viewport_height)) as usize;
        let output_word_count = output_len
            .checked_mul(GPU_SURFACE_OUTPUT_F32_FIELDS)
            .ok_or(RenderError::DimensionTooLarge {
                axis: "f32_output_words",
                value: u64::from(viewport_width) * u64::from(viewport_height),
            })?;
        let output_bytes = checked_buffer_byte_count(
            "bricked camera float32 output",
            output_word_count,
            std::mem::size_of::<u32>(),
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "bricked camera float32 output",
            output_bytes,
        )?;
        validate_general_buffer_bytes(
            &self.device.limits(),
            "bricked camera float32 readback",
            output_bytes,
        )?;
        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-bricked-camera-f32-output"),
            size: output_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-bricked-camera-f32-readback"),
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
            brick_x,
            brick_y,
            brick_z,
            grid_x,
            grid_y,
            grid_z,
            brick_voxel_count,
            0,
        ];
        let params_u32_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-bricked-camera-f32-params-u32"),
                contents: bytemuck::cast_slice(&params_u32),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let params_f32_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-bricked-camera-f32-params-f32"),
                contents: bytemuck::cast_slice(&camera_params),
                usage: wgpu::BufferUsages::STORAGE,
            });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-bricked-camera-f32-bind-group"),
            layout: &self.bricked_camera_f32_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: atlas.values_buffer.as_entire_binding(),
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
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: atlas.page_table_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-bricked-camera-f32-command-encoder"),
            });
        let timestamp =
            self.timestamp_query_pair("mirante4d-bricked-camera-f32-readback-timestamp");
        let timestamp_writes = timestamp
            .as_ref()
            .map(super::readback::GpuTimestampQueryPair::compute_pass_writes);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-bricked-camera-f32-compute-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.bricked_camera_f32_pipeline);
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

        let (output_words, gpu_compute_ns) =
            self.submit_and_read_u32_with_optional_timestamp(encoder, readback_buffer, timestamp)?;
        timings.gpu_compute_ns = gpu_compute_ns;
        let mut missing_voxel_samples = 0_u64;
        let mut coverage = Vec::with_capacity(output_words.len() / GPU_SURFACE_OUTPUT_F32_FIELDS);
        let pixels = output_words
            .chunks_exact(GPU_SURFACE_OUTPUT_F32_FIELDS)
            .map(|record| {
                let marker = f32::from_bits(record[1]);
                missing_voxel_samples += gpu_output_f32_missing_samples(marker);
                coverage.push(u8::from(gpu_output_f32_covered(marker)));
                f32::from_bits(record[0])
            })
            .collect::<Vec<_>>();
        let iso_surface = decode_gpu_iso_surface_f32(
            viewport.width,
            viewport.height,
            &output_words,
            mode_uses_iso_f32(mode),
        )?;
        let dvr_rgba = decode_gpu_dvr_rgba_f32(
            viewport.width,
            viewport.height,
            &output_words,
            mode_uses_dvr_f32(mode),
        )?;
        let input_voxels = resident
            .volume_shape
            .element_count()
            .map_err(RenderError::from)?;
        let frame = frame_diagnostics_f32(input_voxels, &pixels);
        let brick_frame = BrickFrameDiagnosticsF32 {
            frame,
            complete: missing_voxel_samples == 0,
            missing_voxel_samples,
        };
        Ok(GpuBrickedRawOutputF32 {
            raw_words: output_words,
            pixels,
            coverage,
            iso_surface,
            dvr_rgba,
            frame,
            brick_frame,
            timings,
        })
    }

    pub fn render_camera_f32_from_bricks(
        &self,
        resident: &ResidentBrickSetF32,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: CameraFrame,
        viewport: RenderViewport,
        mode: CameraRenderModeF32,
    ) -> Result<GpuMipOutputF32, GpuRenderError> {
        self.render_camera_f32_from_bricks_with_quality(
            resident,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
            mode,
            CameraRenderQuality::voxel_exact(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_camera_f32_from_bricks_output_buffer(
        &self,
        resident: &ResidentBrickSetF32,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: CameraFrame,
        viewport: RenderViewport,
        mode: CameraRenderModeF32,
        quality: CameraRenderQuality,
        output_slot: usize,
    ) -> Result<GpuF32CameraOutputBuffer, GpuRenderError> {
        if resident.bricks().is_empty() {
            return Err(RenderError::InvalidBrickAtlas("resident brick set is empty").into());
        }

        let viewport_width = checked_u32("viewport_width", viewport.width)?;
        let viewport_height = checked_u32("viewport_height", viewport.height)?;
        let shape_x = checked_u32("x", resident.volume_shape.x())?;
        let shape_y = checked_u32("y", resident.volume_shape.y())?;
        let shape_z = checked_u32("z", resident.volume_shape.z())?;
        let upload_started = Instant::now();
        let atlas = self.cached_brick_atlas_f32(resident, brick_shape, brick_grid_shape)?;
        let mut timings = GpuRenderTimings {
            upload_ns: duration_ns_u64(upload_started.elapsed()),
            ..Default::default()
        };
        let brick_x = checked_u32("brick_x", atlas.brick_shape.x())?;
        let brick_y = checked_u32("brick_y", atlas.brick_shape.y())?;
        let brick_z = checked_u32("brick_z", atlas.brick_shape.z())?;
        let grid_x = checked_u32("grid_x", atlas.brick_grid_shape.x())?;
        let grid_y = checked_u32("grid_y", atlas.brick_grid_shape.y())?;
        let grid_z = checked_u32("grid_z", atlas.brick_grid_shape.z())?;
        let brick_voxel_count = checked_u32("brick_voxel_count", atlas.brick_voxel_count)?;

        let mut camera_params =
            camera_grid_params_f32_for_transform(resident.grid_to_world, camera, viewport)?;
        let mode_params = gpu_mode_params_f32_for_transform(resident.grid_to_world, mode)?;
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
        let (_, output_bytes) = f32_camera_output_bytes(viewport)?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "bricked camera float32 display output",
            output_bytes,
        )?;
        let params_u32 = [
            viewport_width,
            viewport_height,
            shape_x,
            shape_y,
            shape_z,
            projection_code(crate::current_camera::projection(camera)),
            mode_params.mode_code,
            mode_params.iso_invert,
            brick_x,
            brick_y,
            brick_z,
            grid_x,
            grid_y,
            grid_z,
            brick_voxel_count,
            0,
        ];
        let params_u32_bytes = checked_buffer_byte_count(
            "bricked camera float32 display u32 parameters",
            params_u32.len(),
            std::mem::size_of::<u32>(),
        )?;
        let params_f32_bytes = checked_buffer_byte_count(
            "bricked camera float32 display f32 parameters",
            camera_params.len(),
            std::mem::size_of::<f32>(),
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "bricked camera float32 display u32 parameters",
            params_u32_bytes,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "bricked camera float32 display f32 parameters",
            params_f32_bytes,
        )?;
        let display_buffers = self.f32_camera_display_output_resources(
            viewport,
            &atlas,
            output_slot,
            GpuF32CameraDisplayBufferSpec {
                output_bytes,
                params_u32_bytes,
                params_f32_bytes,
            },
        )?;
        self.queue.write_buffer(
            &display_buffers.params_u32_buffer,
            0,
            bytemuck::cast_slice(&params_u32),
        );
        self.queue.write_buffer(
            &display_buffers.params_f32_buffer,
            0,
            bytemuck::cast_slice(&camera_params),
        );

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-bricked-camera-f32-display-command-encoder"),
            });
        let timestamp = self.timestamp_query_pair("mirante4d-bricked-camera-f32-display-timestamp");
        let timestamp_writes = timestamp
            .as_ref()
            .map(|timestamp| timestamp.compute_pass_writes());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-bricked-camera-f32-display-compute-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.bricked_camera_f32_pipeline);
            pass.set_bind_group(0, &display_buffers.bind_group, &[]);
            pass.dispatch_workgroups(
                viewport_width.div_ceil(WORKGROUP_SIZE_X),
                viewport_height.div_ceil(WORKGROUP_SIZE_Y),
                1,
            );
        }
        timings.gpu_compute_ns = self.submit_with_optional_timestamp(encoder, timestamp)?;
        Ok(GpuF32CameraOutputBuffer {
            output_buffer: display_buffers.output_buffer,
            output_bytes: display_buffers.output_bytes,
            timings,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render_camera_f32_from_bricks_with_quality(
        &self,
        resident: &ResidentBrickSetF32,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: CameraFrame,
        viewport: RenderViewport,
        mode: CameraRenderModeF32,
        quality: CameraRenderQuality,
    ) -> Result<GpuMipOutputF32, GpuRenderError> {
        let output = self.render_camera_f32_from_bricks_raw_pairs(
            resident,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
            mode,
            quality,
        )?;
        Ok(GpuMipOutputF32 {
            image: MipImageF32::try_new_with_mode_frames(
                viewport.width,
                viewport.height,
                output.pixels,
                PixelCoverage::Mask(output.coverage),
                output.iso_surface,
                output.dvr_rgba,
            )?,
            frame: output.frame,
            brick_frame: Some(output.brick_frame),
            timings: Some(output.timings),
            adapter: self.adapter.clone(),
        })
    }
}

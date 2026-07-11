use std::time::Instant;

use mirante4d_core::{CameraState, GridToWorld, Shape3D};
use wgpu::util::DeviceExt;

use super::{
    GpuMipOutput, GpuRenderError, GpuRenderTimings, GpuRenderer, WORKGROUP_SIZE_X,
    WORKGROUP_SIZE_Y, add_gpu_render_timings,
    buffers::{
        checked_buffer_byte_count, checked_u32, validate_general_buffer_bytes,
        validate_storage_buffer_bytes,
    },
    decode::{
        GPU_SURFACE_OUTPUT_U16_FIELDS, decode_gpu_dvr_rgba_u16, decode_gpu_iso_surface_u16,
        gpu_output_covered, gpu_output_missing_samples, gpu_output_value_u16, mode_uses_dvr_u16,
        mode_uses_iso_u16,
    },
    display::{GpuIntegerCameraOutputBuffer, integer_camera_output_bytes},
    display_resources::GpuIntegerCameraDisplayBufferSpec,
    duration_ns_u64,
    params::{
        GPU_PARAM_DVR_ALPHA_MULTIPLIER_INDEX, GPU_PARAM_DVR_COLOR_B_INDEX,
        GPU_PARAM_DVR_COLOR_G_INDEX, GPU_PARAM_DVR_COLOR_R_INDEX,
        GPU_PARAM_DVR_OPACITY_GAMMA_INDEX, GPU_PARAM_DVR_OPACITY_HIGH_INDEX,
        GPU_PARAM_DVR_OPACITY_LOW_INDEX, GPU_PARAM_ISO_DISPLAY_HIGH_INDEX,
        GPU_PARAM_ISO_DISPLAY_LOW_INDEX, GPU_PARAM_ISO_GAMMA_INDEX, GPU_PARAM_ISO_LEVEL_INDEX,
        apply_gpu_quality_params, camera_grid_params_for_transform, gpu_mode_params_for_transform,
        projection_code,
    },
};
use crate::{
    BrickFrameDiagnostics, BrickSkipDiagnostics, CameraRenderMode, CameraRenderQuality,
    DvrRgbaFrame, IsoSurfaceFrameU16, MipImageU16, PixelCoverage, RenderError, RenderViewport,
    ResidentBrickSetU8, ResidentBrickSetU16, frame_diagnostics,
};

#[derive(Debug, Clone, PartialEq)]
struct GpuBrickedRawOutputU16 {
    packed_pixels: Vec<u32>,
    pixels: Vec<u16>,
    coverage: Vec<u8>,
    iso_surface: Option<IsoSurfaceFrameU16>,
    dvr_rgba: Option<DvrRgbaFrame>,
    frame: crate::FrameDiagnostics,
    brick_frame: BrickFrameDiagnostics,
    timings: GpuRenderTimings,
}

const GPU_BRICK_SKIP_DIAGNOSTIC_WORDS: usize = 5;

fn decode_brick_skip_diagnostics(words: &[u32]) -> BrickSkipDiagnostics {
    BrickSkipDiagnostics {
        skipped_brick_intervals: u64::from(words[0]),
        empty_brick_intervals: u64::from(words[1]),
        mip_range_intervals: u64::from(words[2]),
        iso_range_intervals: u64::from(words[3]),
        dvr_range_intervals: u64::from(words[4]),
    }
}

fn create_brick_skip_diagnostics_buffer(device: &wgpu::Device) -> wgpu::Buffer {
    let zeros = [0_u32; GPU_BRICK_SKIP_DIAGNOSTIC_WORDS];
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("mirante4d-bricked-camera-skip-diagnostics"),
        contents: bytemuck::cast_slice(&zeros),
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
    })
}

impl GpuRenderer {
    pub fn render_camera_u8_from_bricks(
        &self,
        resident: &ResidentBrickSetU8,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: CameraState,
        viewport: RenderViewport,
        mode: CameraRenderMode,
    ) -> Result<GpuMipOutput, GpuRenderError> {
        self.render_camera_u8_from_bricks_with_quality(
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
    pub fn render_camera_u8_from_bricks_with_quality(
        &self,
        resident: &ResidentBrickSetU8,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: CameraState,
        viewport: RenderViewport,
        mode: CameraRenderMode,
        quality: CameraRenderQuality,
    ) -> Result<GpuMipOutput, GpuRenderError> {
        let raw = self.render_camera_u8_from_bricks_raw_u32(
            resident,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
            mode,
            quality,
        )?;
        Ok(GpuMipOutput {
            image: MipImageU16::try_new_with_mode_frames(
                viewport.width,
                viewport.height,
                raw.pixels,
                PixelCoverage::Mask(raw.coverage),
                raw.iso_surface,
                raw.dvr_rgba,
            )?,
            frame: raw.frame,
            brick_frame: Some(raw.brick_frame),
            timings: Some(raw.timings),
            adapter: self.adapter.clone(),
        })
    }

    pub fn render_camera_from_bricks(
        &self,
        resident: &ResidentBrickSetU16,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: CameraState,
        viewport: RenderViewport,
        mode: CameraRenderMode,
    ) -> Result<GpuMipOutput, GpuRenderError> {
        self.render_camera_from_bricks_with_quality(
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
    pub fn render_camera_from_bricks_with_quality(
        &self,
        resident: &ResidentBrickSetU16,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: CameraState,
        viewport: RenderViewport,
        mode: CameraRenderMode,
        quality: CameraRenderQuality,
    ) -> Result<GpuMipOutput, GpuRenderError> {
        let raw = self.render_camera_from_bricks_raw_u32(
            resident,
            brick_shape,
            brick_grid_shape,
            camera,
            viewport,
            mode,
            quality,
        )?;
        Ok(GpuMipOutput {
            image: MipImageU16::try_new_with_mode_frames(
                viewport.width,
                viewport.height,
                raw.pixels,
                PixelCoverage::Mask(raw.coverage),
                raw.iso_surface,
                raw.dvr_rgba,
            )?,
            frame: raw.frame,
            brick_frame: Some(raw.brick_frame),
            timings: Some(raw.timings),
            adapter: self.adapter.clone(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn render_camera_u8_from_bricks_raw_u32(
        &self,
        resident: &ResidentBrickSetU8,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: CameraState,
        viewport: RenderViewport,
        mode: CameraRenderMode,
        quality: CameraRenderQuality,
    ) -> Result<GpuBrickedRawOutputU16, GpuRenderError> {
        if resident.bricks().is_empty() {
            return Err(RenderError::InvalidBrickAtlas("resident uint8 brick set is empty").into());
        }

        let upload_started = Instant::now();
        let atlas = self.cached_brick_atlas_u8(resident, brick_shape, brick_grid_shape)?;
        let timings = GpuRenderTimings {
            upload_ns: duration_ns_u64(upload_started.elapsed()),
            ..Default::default()
        };
        self.render_camera_from_integer_atlas_raw_u32(
            resident.volume_shape,
            resident.grid_to_world,
            atlas,
            timings,
            camera,
            viewport,
            mode,
            quality,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_camera_from_bricks_raw_u32(
        &self,
        resident: &ResidentBrickSetU16,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: CameraState,
        viewport: RenderViewport,
        mode: CameraRenderMode,
        quality: CameraRenderQuality,
    ) -> Result<GpuBrickedRawOutputU16, GpuRenderError> {
        if resident.bricks().is_empty() {
            return Err(RenderError::InvalidBrickAtlas("resident brick set is empty").into());
        }

        let upload_started = Instant::now();
        let atlas = self.cached_brick_atlas(resident, brick_shape, brick_grid_shape)?;
        let timings = GpuRenderTimings {
            upload_ns: duration_ns_u64(upload_started.elapsed()),
            ..Default::default()
        };
        self.render_camera_from_integer_atlas_raw_u32(
            resident.volume_shape,
            resident.grid_to_world,
            atlas,
            timings,
            camera,
            viewport,
            mode,
            quality,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_camera_u8_from_bricks_output_buffer(
        &self,
        resident: &ResidentBrickSetU8,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: CameraState,
        viewport: RenderViewport,
        mode: CameraRenderMode,
        quality: CameraRenderQuality,
        output_slot: usize,
    ) -> Result<GpuIntegerCameraOutputBuffer, GpuRenderError> {
        if resident.bricks().is_empty() {
            return Err(RenderError::InvalidBrickAtlas("resident uint8 brick set is empty").into());
        }

        let upload_started = Instant::now();
        let atlas = self.cached_brick_atlas_u8(resident, brick_shape, brick_grid_shape)?;
        let timings = GpuRenderTimings {
            upload_ns: duration_ns_u64(upload_started.elapsed()),
            ..Default::default()
        };
        self.render_camera_from_integer_atlas_output_buffer(
            resident.volume_shape,
            resident.grid_to_world,
            atlas,
            timings,
            camera,
            viewport,
            mode,
            quality,
            output_slot,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_camera_from_bricks_output_buffer(
        &self,
        resident: &ResidentBrickSetU16,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: CameraState,
        viewport: RenderViewport,
        mode: CameraRenderMode,
        quality: CameraRenderQuality,
        output_slot: usize,
    ) -> Result<GpuIntegerCameraOutputBuffer, GpuRenderError> {
        if resident.bricks().is_empty() {
            return Err(RenderError::InvalidBrickAtlas("resident brick set is empty").into());
        }

        let upload_started = Instant::now();
        let atlas = self.cached_brick_atlas(resident, brick_shape, brick_grid_shape)?;
        let timings = GpuRenderTimings {
            upload_ns: duration_ns_u64(upload_started.elapsed()),
            ..Default::default()
        };
        self.render_camera_from_integer_atlas_output_buffer(
            resident.volume_shape,
            resident.grid_to_world,
            atlas,
            timings,
            camera,
            viewport,
            mode,
            quality,
            output_slot,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_camera_from_integer_atlas_raw_u32(
        &self,
        volume_shape: Shape3D,
        grid_to_world: GridToWorld,
        atlas: super::atlas::GpuBrickAtlasResource,
        mut timings: GpuRenderTimings,
        camera: CameraState,
        viewport: RenderViewport,
        mode: CameraRenderMode,
        quality: CameraRenderQuality,
    ) -> Result<GpuBrickedRawOutputU16, GpuRenderError> {
        let viewport_width = checked_u32("viewport_width", viewport.width)?;
        let viewport_height = checked_u32("viewport_height", viewport.height)?;
        let shape_x = checked_u32("x", volume_shape.x)?;
        let shape_y = checked_u32("y", volume_shape.y)?;
        let shape_z = checked_u32("z", volume_shape.z)?;
        let brick_x = checked_u32("brick_x", atlas.brick_shape.x)?;
        let brick_y = checked_u32("brick_y", atlas.brick_shape.y)?;
        let brick_z = checked_u32("brick_z", atlas.brick_shape.z)?;
        let grid_x = checked_u32("grid_x", atlas.brick_grid_shape.x)?;
        let grid_y = checked_u32("grid_y", atlas.brick_grid_shape.y)?;
        let grid_z = checked_u32("grid_z", atlas.brick_grid_shape.z)?;
        let brick_voxel_count = checked_u32("brick_voxel_count", atlas.brick_voxel_count)?;
        let packed_u32_per_brick = checked_u32("packed_u32_per_brick", atlas.packed_u32_per_brick)?;
        let valid_u32_per_brick = checked_u32("valid_u32_per_brick", atlas.valid_u32_per_brick)?;

        let mut camera_params = camera_grid_params_for_transform(grid_to_world, camera, viewport)?;
        let mode_params = gpu_mode_params_for_transform(grid_to_world, mode)?;
        camera_params[15] = mode_params.density_scale;
        camera_params[GPU_PARAM_ISO_LEVEL_INDEX] = mode_params.iso_display_level;
        camera_params[GPU_PARAM_ISO_DISPLAY_LOW_INDEX] = mode_params.iso_transfer.window.low;
        camera_params[GPU_PARAM_ISO_DISPLAY_HIGH_INDEX] = mode_params.iso_transfer.window.high;
        camera_params[GPU_PARAM_ISO_GAMMA_INDEX] = mode_params.iso_transfer.curve.gamma_value();
        camera_params[GPU_PARAM_DVR_COLOR_R_INDEX] = mode_params.dvr_color_rgb[0];
        camera_params[GPU_PARAM_DVR_COLOR_G_INDEX] = mode_params.dvr_color_rgb[1];
        camera_params[GPU_PARAM_DVR_COLOR_B_INDEX] = mode_params.dvr_color_rgb[2];
        camera_params[GPU_PARAM_DVR_ALPHA_MULTIPLIER_INDEX] = mode_params.dvr_alpha_multiplier;
        camera_params[GPU_PARAM_DVR_OPACITY_LOW_INDEX] =
            mode_params.dvr_opacity_transfer.window.low;
        camera_params[GPU_PARAM_DVR_OPACITY_HIGH_INDEX] =
            mode_params.dvr_opacity_transfer.window.high;
        camera_params[GPU_PARAM_DVR_OPACITY_GAMMA_INDEX] =
            mode_params.dvr_opacity_transfer.curve.gamma_value();
        apply_gpu_quality_params(&mut camera_params, quality);
        let output_len = (u64::from(viewport_width) * u64::from(viewport_height)) as usize;
        let output_bytes = checked_buffer_byte_count(
            "bricked camera uint16 output",
            output_len,
            GPU_SURFACE_OUTPUT_U16_FIELDS * std::mem::size_of::<u32>(),
        )?;
        let skip_diagnostics_bytes = checked_buffer_byte_count(
            "bricked camera skip diagnostics",
            GPU_BRICK_SKIP_DIAGNOSTIC_WORDS,
            std::mem::size_of::<u32>(),
        )?;
        let readback_bytes = output_bytes.checked_add(skip_diagnostics_bytes).ok_or(
            GpuRenderError::BufferSizeOverflow {
                resource: "bricked camera uint16 readback",
            },
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "bricked camera uint16 output",
            output_bytes,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "bricked camera skip diagnostics",
            skip_diagnostics_bytes,
        )?;
        validate_general_buffer_bytes(
            &self.device.limits(),
            "bricked camera uint16 readback",
            readback_bytes,
        )?;
        let output_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-bricked-camera-output-u32"),
            size: output_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let skip_diagnostics_buffer = create_brick_skip_diagnostics_buffer(&self.device);
        let readback_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("mirante4d-bricked-camera-readback-u32"),
            size: readback_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let params_u32 = [
            viewport_width,
            viewport_height,
            shape_x,
            shape_y,
            shape_z,
            projection_code(camera.projection),
            mode_params.mode_code,
            mode_params.iso_invert,
            brick_x,
            brick_y,
            brick_z,
            grid_x,
            grid_y,
            grid_z,
            brick_voxel_count,
            packed_u32_per_brick,
            atlas.values_per_word,
            atlas.bits_per_value,
            atlas.value_mask,
            valid_u32_per_brick,
        ];
        let params_u32_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-bricked-camera-params-u32"),
                contents: bytemuck::cast_slice(&params_u32),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let params_f32_buffer = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("mirante4d-bricked-camera-params-f32"),
                contents: bytemuck::cast_slice(&camera_params),
                usage: wgpu::BufferUsages::STORAGE,
            });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mirante4d-bricked-camera-bind-group"),
            layout: &self.bricked_camera_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: atlas.packed_values_buffer.as_entire_binding(),
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
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: atlas.validity_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: atlas.metadata_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: skip_diagnostics_buffer.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-bricked-camera-command-encoder"),
            });
        let timestamp = self.timestamp_query_pair("mirante4d-bricked-camera-readback-timestamp");
        let timestamp_writes = timestamp
            .as_ref()
            .map(super::readback::GpuTimestampQueryPair::compute_pass_writes);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-bricked-camera-compute-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.bricked_camera_pipeline);
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
        encoder.copy_buffer_to_buffer(
            &skip_diagnostics_buffer,
            0,
            &readback_buffer,
            output_bytes as wgpu::BufferAddress,
            skip_diagnostics_bytes as wgpu::BufferAddress,
        );

        let (readback_u32, gpu_compute_ns) =
            self.submit_and_read_u32_with_optional_timestamp(encoder, readback_buffer, timestamp)?;
        timings.gpu_compute_ns = gpu_compute_ns;
        let output_word_count = usize::try_from(output_bytes / std::mem::size_of::<u32>() as u64)
            .map_err(|_| GpuRenderError::BufferSizeOverflow {
            resource: "bricked camera uint16 output",
        })?;
        let (output_u32, skip_diagnostics_words) = readback_u32.split_at(output_word_count);
        let skip = decode_brick_skip_diagnostics(skip_diagnostics_words);
        let mut missing_voxel_samples = 0_u64;
        let pixels: Vec<u16> = output_u32
            .chunks_exact(GPU_SURFACE_OUTPUT_U16_FIELDS)
            .map(|record| {
                missing_voxel_samples += gpu_output_missing_samples(record[0]);
                gpu_output_value_u16(record[0])
            })
            .collect();
        let coverage = output_u32
            .chunks_exact(GPU_SURFACE_OUTPUT_U16_FIELDS)
            .map(|record| u8::from(gpu_output_covered(record[0])))
            .collect();
        let iso_surface = decode_gpu_iso_surface_u16(
            viewport.width,
            viewport.height,
            output_u32,
            mode_uses_iso_u16(mode),
        )?;
        let dvr_rgba = decode_gpu_dvr_rgba_u16(
            viewport.width,
            viewport.height,
            output_u32,
            mode_uses_dvr_u16(mode),
        )?;
        let input_voxels = volume_shape.element_count().map_err(RenderError::from)?;
        let frame = frame_diagnostics(input_voxels, &pixels);
        let brick_frame = BrickFrameDiagnostics {
            frame,
            complete: missing_voxel_samples == 0,
            missing_voxel_samples,
            skip,
        };
        Ok(GpuBrickedRawOutputU16 {
            packed_pixels: output_u32
                .chunks_exact(GPU_SURFACE_OUTPUT_U16_FIELDS)
                .map(|record| record[0])
                .collect(),
            pixels,
            coverage,
            iso_surface,
            dvr_rgba,
            frame,
            brick_frame,
            timings,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn render_camera_from_integer_atlas_output_buffer(
        &self,
        volume_shape: Shape3D,
        grid_to_world: GridToWorld,
        atlas: super::atlas::GpuBrickAtlasResource,
        mut timings: GpuRenderTimings,
        camera: CameraState,
        viewport: RenderViewport,
        mode: CameraRenderMode,
        quality: CameraRenderQuality,
        output_slot: usize,
    ) -> Result<GpuIntegerCameraOutputBuffer, GpuRenderError> {
        let viewport_width = checked_u32("viewport_width", viewport.width)?;
        let viewport_height = checked_u32("viewport_height", viewport.height)?;
        let shape_x = checked_u32("x", volume_shape.x)?;
        let shape_y = checked_u32("y", volume_shape.y)?;
        let shape_z = checked_u32("z", volume_shape.z)?;
        let brick_x = checked_u32("brick_x", atlas.brick_shape.x)?;
        let brick_y = checked_u32("brick_y", atlas.brick_shape.y)?;
        let brick_z = checked_u32("brick_z", atlas.brick_shape.z)?;
        let grid_x = checked_u32("grid_x", atlas.brick_grid_shape.x)?;
        let grid_y = checked_u32("grid_y", atlas.brick_grid_shape.y)?;
        let grid_z = checked_u32("grid_z", atlas.brick_grid_shape.z)?;
        let brick_voxel_count = checked_u32("brick_voxel_count", atlas.brick_voxel_count)?;
        let packed_u32_per_brick = checked_u32("packed_u32_per_brick", atlas.packed_u32_per_brick)?;
        let valid_u32_per_brick = checked_u32("valid_u32_per_brick", atlas.valid_u32_per_brick)?;

        let mut camera_params = camera_grid_params_for_transform(grid_to_world, camera, viewport)?;
        let mode_params = gpu_mode_params_for_transform(grid_to_world, mode)?;
        camera_params[15] = mode_params.density_scale;
        camera_params[GPU_PARAM_ISO_LEVEL_INDEX] = mode_params.iso_display_level;
        camera_params[GPU_PARAM_ISO_DISPLAY_LOW_INDEX] = mode_params.iso_transfer.window.low;
        camera_params[GPU_PARAM_ISO_DISPLAY_HIGH_INDEX] = mode_params.iso_transfer.window.high;
        camera_params[GPU_PARAM_ISO_GAMMA_INDEX] = mode_params.iso_transfer.curve.gamma_value();
        camera_params[GPU_PARAM_DVR_COLOR_R_INDEX] = mode_params.dvr_color_rgb[0];
        camera_params[GPU_PARAM_DVR_COLOR_G_INDEX] = mode_params.dvr_color_rgb[1];
        camera_params[GPU_PARAM_DVR_COLOR_B_INDEX] = mode_params.dvr_color_rgb[2];
        camera_params[GPU_PARAM_DVR_ALPHA_MULTIPLIER_INDEX] = mode_params.dvr_alpha_multiplier;
        camera_params[GPU_PARAM_DVR_OPACITY_LOW_INDEX] =
            mode_params.dvr_opacity_transfer.window.low;
        camera_params[GPU_PARAM_DVR_OPACITY_HIGH_INDEX] =
            mode_params.dvr_opacity_transfer.window.high;
        camera_params[GPU_PARAM_DVR_OPACITY_GAMMA_INDEX] =
            mode_params.dvr_opacity_transfer.curve.gamma_value();
        apply_gpu_quality_params(&mut camera_params, quality);
        let (_, output_bytes) = integer_camera_output_bytes(viewport)?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "bricked camera uint16 output",
            output_bytes,
        )?;
        let skip_diagnostics_bytes = checked_buffer_byte_count(
            "bricked camera skip diagnostics",
            GPU_BRICK_SKIP_DIAGNOSTIC_WORDS,
            std::mem::size_of::<u32>(),
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "bricked camera skip diagnostics",
            skip_diagnostics_bytes,
        )?;
        let params_u32 = [
            viewport_width,
            viewport_height,
            shape_x,
            shape_y,
            shape_z,
            projection_code(camera.projection),
            mode_params.mode_code,
            mode_params.iso_invert,
            brick_x,
            brick_y,
            brick_z,
            grid_x,
            grid_y,
            grid_z,
            brick_voxel_count,
            packed_u32_per_brick,
            atlas.values_per_word,
            atlas.bits_per_value,
            atlas.value_mask,
            valid_u32_per_brick,
        ];
        let params_u32_bytes = checked_buffer_byte_count(
            "bricked camera display u32 parameters",
            params_u32.len(),
            std::mem::size_of::<u32>(),
        )?;
        let params_f32_bytes = checked_buffer_byte_count(
            "bricked camera display f32 parameters",
            camera_params.len(),
            std::mem::size_of::<f32>(),
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "bricked camera display u32 parameters",
            params_u32_bytes,
        )?;
        validate_storage_buffer_bytes(
            &self.device.limits(),
            "bricked camera display f32 parameters",
            params_f32_bytes,
        )?;
        let display_buffers = self.integer_camera_display_output_resources(
            viewport,
            &atlas,
            output_slot,
            GpuIntegerCameraDisplayBufferSpec {
                output_bytes,
                params_u32_bytes,
                params_f32_bytes,
                skip_diagnostics_bytes,
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
        let skip_diagnostics_zeroes = [0_u32; GPU_BRICK_SKIP_DIAGNOSTIC_WORDS];
        self.queue.write_buffer(
            &display_buffers.skip_diagnostics_buffer,
            0,
            bytemuck::cast_slice(&skip_diagnostics_zeroes),
        );

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("mirante4d-bricked-camera-display-command-encoder"),
            });
        let timestamp = self.timestamp_query_pair("mirante4d-bricked-camera-display-timestamp");
        let timestamp_writes = timestamp
            .as_ref()
            .map(|timestamp| timestamp.compute_pass_writes());
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mirante4d-bricked-camera-display-compute-pass"),
                timestamp_writes,
            });
            pass.set_pipeline(&self.bricked_camera_pipeline);
            pass.set_bind_group(0, &display_buffers.bind_group, &[]);
            pass.dispatch_workgroups(
                viewport_width.div_ceil(WORKGROUP_SIZE_X),
                viewport_height.div_ceil(WORKGROUP_SIZE_Y),
                1,
            );
        }
        timings.gpu_compute_ns = self.submit_with_optional_timestamp(encoder, timestamp)?;
        Ok(GpuIntegerCameraOutputBuffer {
            output_buffer: display_buffers.output_buffer,
            output_bytes: display_buffers.output_bytes,
            timings,
        })
    }

    pub fn render_camera_mip_from_bricks_batched(
        &self,
        resident: &ResidentBrickSetU16,
        brick_shape: Shape3D,
        brick_grid_shape: Shape3D,
        camera: CameraState,
        viewport: RenderViewport,
        max_bricks_per_batch: usize,
    ) -> Result<GpuMipOutput, GpuRenderError> {
        if max_bricks_per_batch == 0 {
            return Err(RenderError::InvalidBrickAtlas(
                "batched MIP requires at least one brick per batch",
            )
            .into());
        }
        if resident.bricks().is_empty() {
            return Err(RenderError::InvalidBrickAtlas("resident brick set is empty").into());
        }
        if resident.bricks().len() <= max_bricks_per_batch {
            return self.render_camera_from_bricks(
                resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                CameraRenderMode::Mip,
            );
        }

        let pixel_count = (viewport.width as usize)
            .checked_mul(viewport.height as usize)
            .ok_or(GpuRenderError::BufferSizeOverflow {
                resource: "batched MIP output",
            })?;
        let mut combined_pixels = vec![0u16; pixel_count];
        let mut combined_coverage = vec![0u8; pixel_count];
        let mut timings = GpuRenderTimings::default();
        let mut skip = BrickSkipDiagnostics::default();
        for batch in resident.bricks().chunks(max_bricks_per_batch) {
            let batch_resident = ResidentBrickSetU16::new(
                resident.layer_id.clone(),
                resident.timepoint,
                resident.volume_shape,
                resident.grid_to_world,
                batch.to_vec(),
            );
            let output = self.render_camera_from_bricks(
                &batch_resident,
                brick_shape,
                brick_grid_shape,
                camera,
                viewport,
                CameraRenderMode::Mip,
            )?;
            timings = add_gpu_render_timings(timings, output.timings.unwrap_or_default());
            if let Some(diagnostics) = output.brick_frame {
                skip.add_assign(diagnostics.skip);
            }
            for (index, (combined, value)) in combined_pixels
                .iter_mut()
                .zip(output.image.pixels())
                .enumerate()
            {
                if output.image.is_covered_index(index) {
                    *combined = (*combined).max(*value);
                    combined_coverage[index] = 1;
                }
            }
        }

        let input_voxels = resident
            .volume_shape
            .element_count()
            .map_err(RenderError::from)?;
        let frame = frame_diagnostics(input_voxels, &combined_pixels);
        let brick_frame = BrickFrameDiagnostics {
            frame,
            complete: true,
            missing_voxel_samples: 0,
            skip,
        };
        Ok(GpuMipOutput {
            image: MipImageU16::try_new(
                viewport.width,
                viewport.height,
                combined_pixels,
                PixelCoverage::Mask(combined_coverage),
            )?,
            frame,
            brick_frame: Some(brick_frame),
            timings: Some(timings),
            adapter: self.adapter.clone(),
        })
    }
}
